//! «Настройки → Клавиатура» (S5-09): реестр команд (`commands.rs`) с клавишами, поиском и конфликтами; своё
//! сочетание — записью нажатия («Изменить», затем нажать), хранится в настройках поверх реестра и действует сразу
//! (`commands::bind`). Сброс — у команды и всех. Расчёты — `keymap.rs`.
//!
//! Схема (S5-17, макет `SettingsKeys`): «Outlook» по умолчанию или «Одиночные клавиши»; столбцы обеих схем, правится
//! столбец выбранной, у каждого — свои сочетания.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_commands::Command;
use pets_i18n::tr;
use pets_ui::theme::Metric;

use postman_core::prefs::KeyScheme;

use crate::buttons::button;
use crate::commands::SINGLE;
use crate::keymap::{self, Column, KeySet, Place, human};
use crate::roles::Look;
use crate::settings_tab::{SettingsView, aside_card, keycap, link, seg, seg_group};

/// Состояние раздела.
pub(crate) struct KeysState {
    search: Entity<InputState>,
    query: String,
    /// Команда, для которой записывается сочетание.
    pub(crate) recording: Option<&'static str>,
    /// Нажатое сочетание занято другой командой: (сочетание, у кого).
    pub(crate) clash: Option<(String, &'static str)>,
    /// Перехват нажатий на время записи (клавиши не уходят командам).
    intercept: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl KeysState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-keys-search")));
        let sub = cx.subscribe_in(&search, window, |this: &mut SettingsView, state, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                this.keys.query = state.read(cx).value().to_string();
                cx.notify();
            }
        });
        Self { search, query: String::new(), recording: None, clash: None, intercept: None, _subscriptions: vec![sub] }
    }
}

/// Название команды на языке интерфейса.
pub(crate) fn command_name(c: &Command) -> String {
    tr!(&c.name_key())
}

impl SettingsView {
    /// «Изменить»: следующее нажатие (не одни модификаторы) — новое сочетание команды; Esc — отмена.
    fn start_recording(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.keys.recording = Some(id);
        self.keys.clash = None;
        let view = cx.entity().downgrade();
        self.keys.intercept = Some(cx.intercept_keystrokes(move |ev, _window, cx| {
            let keystroke = ev.keystroke.unparse();
            let taken = view.update(cx, |v, cx| v.on_keystroke(&keystroke, cx)).unwrap_or(false);
            if taken {
                cx.stop_propagation();
            }
        }));
        cx.notify();
    }

    pub(crate) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.keys.recording.take().is_some() || self.keys.intercept.is_some() {
            self.keys.intercept = None;
            self.keys.clash = None;
            cx.notify();
        }
    }

    /// Нажатие во время записи; `true` — забрали (команде не отдавать).
    fn on_keystroke(&mut self, keystroke: &str, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.keys.recording else { return false };
        if keystroke == "escape" {
            self.stop_recording(cx);
            return true;
        }
        let Some(keys) = keymap::recorded(keystroke) else { return true };
        let set = KeySet::of(&crate::prefs::get(cx));
        match keymap::taken_by(crate::commands::all(), &SINGLE, &set, id, &keys) {
            Some(other) => {
                // Занято: решает пользователь («Заменить» или «Оставить оба»); запись окончена.
                self.keys.clash = Some((keys, other));
                self.keys.intercept = None;
                cx.notify();
            }
            None => self.assign(id, &keys, false, cx),
        }
        true
    }

    fn assign(&mut self, id: &'static str, keys: &str, replace: bool, cx: &mut Context<Self>) {
        let mut result = Ok(());
        crate::prefs::update(cx, |p| {
            let mut set = KeySet::of(p);
            result = keymap::assign(crate::commands::all(), &SINGLE, &mut set, id, keys, replace);
            set.store(p);
        });
        if let Err(e) = result {
            tracing::warn!("keys: {id}: {e}");
        }
        self.keys.recording = None;
        self.keys.clash = None;
        self.keys.intercept = None;
        cx.notify();
    }

    pub(crate) fn keyboard(&mut self, l: &Look, cx: &mut Context<Self>) -> (Option<Stateful<Div>>, AnyElement, Vec<Div>) {
        let set = KeySet::of(&crate::prefs::get(cx));
        let reset_all = (!set.outlook.is_empty() || !set.single.is_empty()).then(|| {
            button("settings-keys-reset-all", tr!("settings-keys-reset-all"), false, l).on_click(cx.listener(|this, _, _, cx| {
                this.stop_recording(cx);
                crate::prefs::update(cx, |p| {
                    p.keys.clear();
                    p.keys_single.clear();
                });
                cx.notify();
            }))
        });
        let conflicts = keymap::conflicts(crate::commands::all(), &SINGLE, &set);
        let commands = keymap::search(&self.keys.query, command_name, &SINGLE, &set);
        let single = set.scheme == KeyScheme::Single;
        let column_head =
            |text: String, on: bool, w: f32| div().w(px(w)).when(on, |d| d.text_color(l.text()).font_weight(FontWeight::SEMIBOLD)).child(text);
        let mut table = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .py_2()
                .border_b_1()
                .border_color(l.border())
                .text_size(px(12.))
                .text_color(l.text2())
                .child(div().flex_1().child(tr!("settings-keys-command")))
                .child(column_head(tr!("settings-keys-col-outlook"), !single, W_OUTLOOK))
                .child(column_head(tr!("settings-keys-col-single"), single, W_SINGLE))
                .child(div().w(px(120.)).child(tr!("settings-keys-where")))
                .child(div().w(px(140.))),
        );
        let mut area = "";
        for (i, c) in commands.iter().copied().enumerate() {
            if c.area() != area {
                area = c.area();
                table = table.child(
                    div()
                        .pt_3()
                        .pb_1()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!(&format!("settings-keys-area-{area}"))))
                        .child(div().text_size(px(11.)).text_color(l.text3()).child(format!("{area}.*"))),
                );
            }
            table = table.child(self.key_row(i, c, &set, &conflicts, l, cx));
        }
        if commands.is_empty() {
            table = table.child(div().py_3().text_color(l.text2()).child(tr!("settings-keys-nothing")));
        }
        let mut conflict_list = div().flex().flex_col().gap_1();
        for c in &conflicts {
            let name = |id: &str| crate::commands::all().iter().find(|x| x.id == id).map(command_name).unwrap_or_default();
            conflict_list = conflict_list.child(div().text_color(l.warning()).child(tr!(
                "settings-keys-conflict",
                keys = human(&c.keys),
                first = name(c.first),
                second = name(c.second)
            )));
        }
        // Конфликты — над таблицей (таблице нужна вся ширина), раскладка — под ней.
        let conflicts_block = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded(l.px(Metric::RadiusOverlay))
            .when(conflicts.is_empty(), |d| d.bg(l.fill_passive()))
            .when(!conflicts.is_empty(), |d| d.bg(l.warning_bg()))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("settings-keys-conflicts", n = conflicts.len())))
            .when(conflicts.is_empty(), |d| d.child(div().text_color(l.text2()).child(tr!("settings-keys-no-conflicts"))))
            .child(conflict_list);
        let scheme = |id: &'static str, label: String, s: KeyScheme| {
            seg(id, label, set.scheme == s, true, l).on_click(cx.listener(move |this, _, _, cx| {
                this.stop_recording(cx);
                crate::prefs::update(cx, |p| p.key_scheme = s);
                cx.notify();
            }))
        };
        let schemes = seg_group(
            [
                scheme("settings-keys-scheme-outlook", tr!("settings-keys-scheme-outlook"), KeyScheme::Outlook),
                scheme("settings-keys-scheme-single", tr!("settings-keys-scheme-single"), KeyScheme::Single),
            ],
            l,
        );
        let note = if single { tr!("settings-keys-note-single") } else { tr!("settings-keys-note-outlook") };
        let body = div()
            .flex()
            .flex_col()
            .pt_3()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("settings-keys-scheme")))
                    .child(schemes)
                    .child(div().flex_1())
                    .child(crate::chrome::icon(crate::chrome::glyph::SEARCH, 14.).text_color(l.text2()))
                    .child(div().w(px(320.)).child(Input::new(&self.keys.search))),
            )
            .child(div().text_size(px(12.)).text_color(l.text2()).child(note))
            .child(conflicts_block)
            .child(table)
            .child(aside_card(tr!("settings-keys-layout"), vec![tr!("settings-keys-layout-1"), tr!("settings-keys-layout-2")], l));
        (reset_all, body.into_any_element(), Vec::new())
    }

    fn key_row(&self, i: usize, c: &'static Command, set: &KeySet, conflicts: &[keymap::Conflict], l: &Look, cx: &mut Context<Self>) -> Div {
        let changed = keymap::changed(set, c.id);
        let editable = keymap::editable(c);
        let recording = self.keys.recording == Some(c.id);
        let in_conflict = conflicts.iter().any(|x| x.first == c.id || x.second == c.id);
        let active = set.column();
        // Клавиши столбца: правится столбец выбранной схемы, другой — приглушён.
        let caps = |col: Column| {
            let keys = keymap::column_keys(&SINGLE, set, c, col);
            let mut caps = div().flex().flex_wrap().items_center().gap_1().when(col != active, |d| d.opacity(0.45));
            if recording && col == active && self.keys.clash.is_none() {
                caps = caps.child(div().text_color(l.accent_text()).child(tr!("settings-keys-press")));
            } else if !editable && col == Column::Outlook {
                caps = caps.child(keycap(tr!("settings-keys-digits"), l));
            } else if keys.is_empty() && col == Column::Single {
                // Как в макете: у слоя схемы большинство команд без буквы — тире, а не надпись.
                caps = caps.child(div().text_color(l.text3()).child("\u{2014}"));
            } else if keys.is_empty() {
                caps = caps.child(div().text_color(l.text3()).child(tr!("settings-keys-none")));
            } else {
                for (k, key) in keys.iter().enumerate() {
                    if k > 0 {
                        caps = caps.child(div().text_size(px(11.)).text_color(l.text3()).child(tr!("settings-keys-or")));
                    }
                    caps = caps.child(keycap(human(key), l));
                }
            }
            caps
        };
        let place = Place::of(c.context);
        let id = c.id;
        let actions = div()
            .w(px(140.))
            .flex()
            .justify_end()
            .gap_1()
            .when(changed && editable, |d| {
                d.child(link(("settings-keys-reset", i), tr!("settings-keys-reset"), l).on_click(cx.listener(move |this, _, _, cx| {
                    this.stop_recording(cx);
                    crate::prefs::update(cx, |p| {
                        let mut set = KeySet::of(p);
                        keymap::reset(&mut set, id);
                        set.store(p);
                    });
                    cx.notify();
                })))
            })
            .when(editable && !recording, |d| {
                d.child(
                    link(("settings-keys-edit", i), tr!("settings-edit"), l)
                        .on_click(cx.listener(move |this, _, _, cx| this.start_recording(id, cx))),
                )
            })
            .when(recording, |d| {
                d.child(link(("settings-keys-stop", i), tr!("settings-cancel"), l).on_click(cx.listener(|this, _, _, cx| this.stop_recording(cx))))
            });
        let line = div()
            .min_h(px(36.))
            .flex()
            .items_center()
            .gap_3()
            .py_1()
            .when(recording, |d| d.bg(l.picked()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().min_w_0().truncate().child(command_name(c)))
                            .when(changed, |d| {
                                d.child(
                                    div()
                                        .px_1p5()
                                        .rounded_sm()
                                        .bg(l.picked())
                                        .text_size(px(11.))
                                        .text_color(l.accent_text())
                                        .child(tr!("settings-keys-changed")),
                                )
                            })
                            .when(in_conflict, |d| {
                                d.child(
                                    div()
                                        .px_1p5()
                                        .rounded_sm()
                                        .bg(l.warning_bg())
                                        .text_size(px(11.))
                                        .text_color(l.warning())
                                        .child(tr!("settings-keys-conflict-badge")),
                                )
                            }),
                    )
                    .child(div().text_size(px(11.)).text_color(l.text3()).child(c.id)),
            )
            .child(div().w(px(W_OUTLOOK)).child(caps(Column::Outlook)))
            .child(div().w(px(W_SINGLE)).child(caps(Column::Single)))
            .child(div().w(px(120.)).text_color(l.text2()).child(tr!(place.key())))
            .child(actions);
        let mut wrap = div().flex().flex_col().border_b_1().border_color(l.border()).child(line);
        if recording && let Some((keys, other)) = self.keys.clash.clone() {
            let other_name = crate::commands::all().iter().find(|x| x.id == other).map(command_name).unwrap_or_default();
            let (k1, k2) = (keys.clone(), keys.clone());
            wrap = wrap.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_2()
                    .mb_1()
                    .rounded(l.px(Metric::RadiusControl))
                    .bg(l.warning_bg())
                    .child(div().flex_1().child(tr!("settings-keys-taken", keys = human(&keys), name = other_name)))
                    .child(
                        button(("settings-keys-replace", i), tr!("settings-keys-replace"), true, l)
                            .on_click(cx.listener(move |this, _, _, cx| this.assign(id, &k1, true, cx))),
                    )
                    .child(
                        link(("settings-keys-both", i), tr!("settings-keys-both"), l)
                            .on_click(cx.listener(move |this, _, _, cx| this.assign(id, &k2, false, cx))),
                    )
                    .child(
                        link(("settings-keys-cancel", i), tr!("settings-cancel"), l).on_click(cx.listener(|this, _, _, cx| this.stop_recording(cx))),
                    ),
            );
        }
        wrap
    }
}

/// Ширина столбцов клавиш: Outlook и «Одиночные клавиши» (макет `SettingsKeys`).
const W_OUTLOOK: f32 = 200.;
const W_SINGLE: f32 = 140.;
