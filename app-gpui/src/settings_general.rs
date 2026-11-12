//! «Настройки → Общие» (S5-30, макет `SettingsGeneral`): язык, автозапуск, стартовый экран, почтовый клиент по
//! умолчанию (`mailto:`), уведомления и тихие часы, обновления, журнал и отчёт для поддержки.
//!
//! Действуют сразу: язык, автозапуск (HKCU Run), регистрация `mailto:` (выбор — в «Приложениях по умолчанию»),
//! открыть папку журнала, отчёт. Помечены «сохраняется» (выбор запомнен, действия пока нет): стартовый экран
//! «Последняя вкладка», уведомления Windows, тихие часы, звук. Уровень журнала — со следующего запуска (если
//! `POSTMAN_LOG` не задан). Обновления (решение 2026-11-11 «self-update») — канал, «Проверять автоматически»,
//! «Проверить обновления» и «Установить» — модуль `update`.

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_core::prefs::{Channel, LANGUAGES, LogLevel, Notify, OnClose, QUIET_FROM, QUIET_TO, StartView};

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsView, group, row, seg, seg_group, toggle};

/// Что видно в разделе «Общие» о Windows: автозапуск, клиент по умолчанию, отчёт (читается в фоне).
#[derive(Clone, Default)]
pub(crate) struct OsState {
    pub(crate) autostart: Option<bool>,
    pub(crate) mailto_default: Option<bool>,
    pub(crate) error: Option<String>,
    /// Собранный отчёт для поддержки.
    pub(crate) report: Option<std::path::PathBuf>,
}

impl SettingsView {
    /// Состояние Windows для «Общих» — в фоне (реестр).
    pub(crate) fn read_os(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { (crate::sys::shell::autostart(), crate::sys::shell::mailto_is_default()) });
        cx.spawn(async move |this, cx| {
            let (auto, mailto) = task.await;
            let _ = this.update(cx, |v, cx| {
                v.os.autostart = Some(auto);
                v.os.mailto_default = Some(mailto);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_autostart(&mut self, on: bool, cx: &mut Context<Self>) {
        crate::prefs::update(cx, |p| p.general.autostart = on);
        self.os.autostart = Some(on);
        let task = cx.background_spawn(async move { crate::sys::shell::set_autostart(on) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if let Err(e) = result {
                    tracing::warn!("autostart: {e}");
                    v.os.error = Some(tr!("settings-os-failed"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// «Сделать Postman клиентом по умолчанию»: зарегистрировать и открыть «Приложения по умолчанию» Windows; на macOS
    /// (MAC-02) — сразу выбор LaunchServices, система спросит подтверждение.
    fn make_default(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            crate::sys::shell::register_mailto()?;
            crate::sys::shell::show_default_apps()
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if let Err(e) = result {
                    tracing::warn!("mailto: {e}");
                    v.os.error = Some(tr!("settings-os-failed"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn build_report(&mut self, cx: &mut Context<Self>) {
        let prefs = crate::prefs::get(cx);
        let task = cx.background_spawn(async move {
            let out = crate::diagnostics::report_path();
            crate::diagnostics::build_report(&crate::data_dir(), &prefs, &out).map(|()| out)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(path) => v.os.report = Some(path),
                    Err(e) => {
                        tracing::warn!("report: {e}");
                        v.os.error = Some(tr!("settings-os-failed"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// «Предпросмотр» (T2-58): уведомление о новом письме, как его покажет Windows, — или почему его не будет.
    pub(crate) fn notify_preview(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let g = crate::prefs::get(cx).general;
        let hour = jiff::Zoned::now().hour() as u8;
        let quiet = g.quiet && postman_core::newmail::quiet(hour, g.quiet_from, g.quiet_to);
        let (title, note) = match g.notify {
            Notify::Off => (tr!("settings-notify-preview-off"), tr!("settings-notify-off-hint")),
            _ if quiet => (tr!("settings-notify-preview-quiet"), tr!("settings-quiet-hint")),
            Notify::Important => (tr!("settings-notify-important"), tr!("settings-notify-important-hint")),
            Notify::All => (tr!("settings-notify-all"), tr!("settings-notify-all-hint")),
        };
        let shown = g.notify != Notify::Off && !quiet;
        let toast = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded(l.px(pets_ui::theme::Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.c(pets_ui::theme::Role::LayerOverlay))
            .when(!shown, |d| d.opacity(0.45))
            .child(div().flex().text_size(px(12.)).text_color(l.text2()).child(div().flex_1().child("Postman")).child(tr!(if g.sound && shown {
                "settings-notify-preview-sound"
            } else {
                "settings-notify-preview-silent"
            })))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("settings-notify-preview-from")))
            .child(div().text_size(px(12.)).child(tr!("settings-notify-preview-subject")));
        crate::settings_tab::preview_card(toast, title, note, l)
    }

    pub(crate) fn general(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = crate::prefs::get(cx);
        let g = p.general.clone();
        // Язык.
        let lang = g.language.clone();
        let mut langs = vec![seg("settings-lang-auto", tr!(crate::sys::os_key("settings-lang-auto")), lang.is_none(), true, l).on_click(
            cx.listener(|_, _, _, cx| {
                crate::prefs::set_language(None, cx);
                cx.notify();
            }),
        )];
        for (i, code) in LANGUAGES.iter().copied().enumerate() {
            let on = lang.as_deref() == Some(code);
            langs.push(seg(("settings-lang", i), tr!(&format!("settings-lang-{code}")), on, true, l).on_click(cx.listener(move |_, _, _, cx| {
                crate::prefs::set_language(Some(code), cx);
                cx.notify();
            })));
        }
        let auto = self.os.autostart.unwrap_or(g.autostart);
        let starts = [(StartView::Inbox, "settings-start-inbox"), (StartView::LastTabs, "settings-start-last")].map(|(v, key)| {
            seg(("settings-start", v as usize), tr!(key), g.start == v, true, l).on_click(cx.listener(move |_, _, _, cx| {
                crate::prefs::update(cx, |p| p.general.start = v);
                cx.notify();
            }))
        });
        let start_hint = tr!(if g.start == StartView::Inbox { "settings-start-inbox-hint" } else { "settings-start-last-hint" });
        let start_control = seg_group(starts, l);
        // «Последняя вкладка» действует (T2-11): вкладки — в настройках при каждой смене.
        let start_row = row(tr!("settings-start"), Some(start_hint), start_control, l);
        // Клиент по умолчанию.
        let is_default = self.os.mailto_default == Some(true);
        let mailto_control = if is_default {
            div().text_color(l.text2()).child(tr!("settings-mailto-done")).into_any_element()
        } else {
            button("settings-mailto", tr!("settings-mailto-make"), false, l)
                .on_click(cx.listener(|this, _, _, cx| this.make_default(cx)))
                .into_any_element()
        };
        // Закрытие окна (T3-13): в трей или выход; значок в трее — только Windows.
        let closes = [(OnClose::Tray, "settings-on-close-tray"), (OnClose::Quit, "settings-on-close-quit")].map(|(v, key)| {
            seg(("settings-on-close", v as usize), tr!(key), g.on_close == v, true, l).on_click(cx.listener(move |_, _, _, cx| {
                crate::prefs::update(cx, |p| p.general.on_close = v);
                cx.notify();
            }))
        });
        let close_hint = tr!(if g.on_close == OnClose::Tray { "settings-on-close-tray-hint" } else { "settings-on-close-quit-hint" });
        // Уведомления.
        let notifies = [(Notify::All, "settings-notify-all"), (Notify::Important, "settings-notify-important"), (Notify::Off, "settings-notify-off")]
            .map(|(v, key)| {
                seg(("settings-notify", v as usize), tr!(key), g.notify == v, true, l).on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.general.notify = v);
                    cx.notify();
                }))
            });
        let notify_hint = tr!(match g.notify {
            Notify::All => "settings-notify-all-hint",
            Notify::Important => "settings-notify-important-hint",
            Notify::Off => "settings-notify-off-hint",
        });
        let on = g.notify != Notify::Off;
        let hours = |id: &'static str, list: &'static [u8], current: u8, set: fn(&mut postman_core::prefs::General, u8)| {
            seg_group(
                list.iter().copied().map(|h| {
                    seg((id, h as usize), format!("{h:02}:00"), current == h, on, l).when(on, |d| {
                        d.on_click(cx.listener(move |_, _, _, cx| {
                            crate::prefs::update(cx, |p| set(&mut p.general, h));
                            cx.notify();
                        }))
                    })
                }),
                l,
            )
        };
        let quiet_from = hours("settings-quiet-from", &QUIET_FROM, g.quiet_from, |g, h| g.quiet_from = h);
        let quiet_to = hours("settings-quiet-to", &QUIET_TO, g.quiet_to, |g, h| g.quiet_to = h);
        let quiet = div()
            .flex()
            .items_center()
            .gap_2()
            .child(toggle("settings-quiet", g.quiet, l).on_click(cx.listener(|_, _, _, cx| {
                crate::prefs::update(cx, |p| p.general.quiet = !p.general.quiet);
                cx.notify();
            })))
            .when(g.quiet, |d| d.child(quiet_from).child("—").child(quiet_to));
        // Обновления и журнал.
        let channels = [(Channel::Stable, "settings-channel-stable"), (Channel::Preview, "settings-channel-preview")].map(|(v, key)| {
            seg(("settings-channel", v as usize), tr!(key), g.channel == v, true, l).on_click(cx.listener(move |_, _, _, cx| {
                crate::prefs::update(cx, |p| p.general.channel = v);
                cx.notify();
            }))
        });
        // Самообновление (решение 2026-11-11 «self-update»): выключатель; при политике `AllowUpdate = 0` — заперт, кнопки
        // «Проверить» нет. Страница выпусков — второй ссылкой.
        let upd = crate::update::view(cx);
        let auto_check = g.auto_update && upd.allowed;
        let auto_hint = if upd.allowed {
            tr!(if auto_check { "settings-auto-update-on-hint" } else { "settings-auto-update-off-hint" })
        } else {
            format!("{} {}", tr!("update-policy"), tr!("settings-policy-pinned"))
        };
        let auto_control = if upd.allowed {
            toggle("settings-auto-update", auto_check, l)
                .on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.general.auto_update = !p.general.auto_update);
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div().opacity(0.6).child(crate::settings_tab::switch(false, l)).into_any_element()
        };
        let auto_update_row = row(tr!("settings-auto-update"), Some(auto_hint), auto_control, l);
        let busy =
            matches!(upd.status, crate::update::Status::Checking | crate::update::Status::Downloading(_) | crate::update::Status::Installing(_));
        let channel = g.channel;
        let check_control = div()
            .flex()
            .gap_2()
            .child(crate::settings_tab::link("settings-releases", tr!("settings-releases"), l).on_click(cx.listener(move |_, _, _, _| {
                if let Err(e) = crate::sys::shell::open(&crate::diagnostics::releases(channel)) {
                    tracing::warn!("updates: {e}");
                }
            })))
            .when_some(upd.offer.clone().filter(|_| upd.allowed && !busy), |d, _| {
                d.child(button("settings-update-install", tr!("update-install"), true, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::update::install(cx);
                    cx.notify();
                })))
            })
            .when(upd.allowed, |d| {
                d.child(button("settings-check", tr!("settings-check"), false, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::update::check_now(cx);
                    cx.notify();
                })))
            });
        let logs = LogLevel::ALL.map(|v| {
            seg(("settings-log", v as usize), tr!(&format!("settings-log-{}", v.filter())), g.log == v, true, l).on_click(cx.listener(
                move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.general.log = v);
                    cx.notify();
                },
            ))
        });
        let log_dir = crate::diagnostics::log_dir(&crate::data_dir());
        let log_size = crate::diagnostics::folder_size(&log_dir);
        let report_hint = match &self.os.report {
            Some(path) => tr!("settings-report-done", file = path.display().to_string()),
            None => tr!("settings-report-hint"),
        };
        let report_control = div()
            .flex()
            .gap_2()
            .when_some(self.os.report.clone(), |d, path| {
                d.child(button("settings-report-show", tr!("settings-report-show"), false, l).on_click(cx.listener(move |_, _, _, _| {
                    if let Err(e) = crate::sys::shell::reveal(&path) {
                        tracing::warn!("report: {e}");
                    }
                })))
            })
            .child(button("settings-report", tr!("settings-report-make"), false, l).on_click(cx.listener(|this, _, _, cx| this.build_report(cx))));
        div()
            .flex()
            .flex_col()
            .when_some(self.os.error.clone(), |d, e| d.child(div().pt_3().text_color(l.error()).child(e)))
            .child(group(tr!("settings-language-group"), l))
            .child(row(tr!("settings-language"), Some(tr!("settings-language-hint")), seg_group(langs, l), l))
            .child(row(
                tr!(crate::sys::os_key("settings-autostart")),
                Some(tr!(crate::sys::os_key(if auto { "settings-autostart-on" } else { "settings-autostart-off" }))),
                toggle("settings-autostart", auto, l).on_click(cx.listener(move |this, _, _, cx| this.set_autostart(!auto, cx))),
                l,
            ))
            .when(cfg!(windows), |d| d.child(row(tr!("settings-on-close"), Some(close_hint), seg_group(closes, l), l)))
            .child(start_row)
            .child(group(tr!("settings-mailto-group"), l))
            .child(row(
                tr!("settings-mailto"),
                Some(tr!(crate::sys::os_key(if is_default { "settings-mailto-done-hint" } else { "settings-mailto-hint" }))),
                mailto_control,
                l,
            ))
            .child(group(tr!(crate::sys::os_key("settings-notify-group")), l))
            .child(row(tr!("settings-notify"), Some(notify_hint), seg_group(notifies, l), l))
            .when(on, |d| {
                d.child(row(tr!("settings-quiet"), Some(tr!("settings-quiet-hint")), quiet, l)).child(row(
                    tr!("settings-sound"),
                    Some(tr!(crate::sys::os_key("settings-sound-hint"))),
                    toggle("settings-sound", g.sound, l).on_click(cx.listener(|_, _, _, cx| {
                        crate::prefs::update(cx, |p| p.general.sound = !p.general.sound);
                        cx.notify();
                    })),
                    l,
                ))
            })
            .child(group(tr!("settings-updates-group"), l))
            .child(row(
                tr!("settings-channel"),
                Some(tr!(if g.channel == Channel::Stable { "settings-channel-stable-hint" } else { "settings-channel-preview-hint" })),
                seg_group(channels, l),
                l,
            ))
            .child(auto_update_row)
            .child(row(tr!("settings-version", version = env!("CARGO_PKG_VERSION")), Some(crate::update::status_text(&upd)), check_control, l))
            .child(group(tr!("settings-log-group"), l))
            .child(row(tr!("settings-log"), Some(tr!("settings-log-hint")), seg_group(logs, l), l))
            .child(row(
                tr!("settings-log-folder"),
                Some(tr!("settings-log-folder-hint", path = log_dir.display().to_string(), mb = log_size.div_ceil(1_048_576))),
                button("settings-log-open", tr!("settings-log-open"), false, l).on_click(cx.listener(move |_, _, _, _| {
                    let _ = std::fs::create_dir_all(&log_dir);
                    if let Err(e) = crate::sys::shell::open(&log_dir.display().to_string()) {
                        tracing::warn!("log folder: {e}");
                    }
                })),
                l,
            ))
            .child(row(tr!("settings-report"), Some(report_hint), report_control, l))
    }
}
