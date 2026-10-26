//! «Настройки → Безопасность» (S5-30, макет `SettingsSecurity`): содержимое писем, вложения, песочница, фишинг, входы
//! и соединение. Выбор — только строже прежнего (решение владельца 2026-10-22): картинки «никогда» или «спрашивать»,
//! TLS 1.2 или 1.3, пароль — в Диспетчере учётных данных или не хранить; проверка отзыва — всегда (выключатель
//! закреплён). Политика организации закрепляет значения (`security.rs`). Файлы с макросами не открываются (решение
//! №7); «Сохранить…» — настройка.

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_core::prefs::Images;
use postman_worker::Level;

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsView, group, link, row, seg, seg_group, toggle};

impl SettingsView {
    /// «Сбросить»: пароль учётной записи — из Диспетчера учётных данных (в фоне); вход спросим при следующей
    /// синхронизации.
    pub(crate) fn wipe_sign_ins(&mut self, cx: &mut Context<Self>) {
        self.wipe_confirm = false;
        let Some(account) = self.info.account.clone() else { return };
        let task = cx.background_spawn(async move { crate::sys::credentials::delete(&account.credential_target()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(()) => {
                        v.stored = Some(false);
                        v.wiped = true;
                    }
                    Err(e) => {
                        tracing::warn!("credential manager: {e}");
                        v.os.error = Some(tr!("settings-os-failed"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn security(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = crate::prefs::get(cx);
        let fixed = |id: &'static str, key: &str| seg_group([seg(id, tr!(key), true, false, l)], l);
        let policy = crate::security::policy();
        let eff = crate::security::effective(&p);
        // Закреплено политикой — выбор недоступен, подсказка говорит об этом.
        let pinned = |hint: String, by_policy: bool| if by_policy { format!("{hint} {}", tr!("settings-policy-pinned")) } else { hint };
        let images = seg_group(
            [(Images::Never, "settings-images-never"), (Images::Ask, "settings-images-ask")].map(|(v, key)| {
                let enabled = policy.images.is_none();
                seg(("settings-images", v as usize), tr!(key), eff.images == v, enabled, l).when(enabled, |d| {
                    d.on_click(cx.listener(move |_, _, _, cx| {
                        crate::prefs::update(cx, |p| p.security.images = v);
                        cx.notify();
                    }))
                })
            }),
            l,
        );
        let tls = seg_group(
            [(false, "settings-tls-12"), (true, "settings-tls-13")].map(|(v, key)| {
                let enabled = policy.tls13.is_none();
                seg(("settings-tls", v as usize), tr!(key), eff.tls13 == v, enabled, l).when(enabled, |d| {
                    d.on_click(cx.listener(move |_, _, _, cx| {
                        crate::prefs::update(cx, |p| p.security.tls13 = v);
                        crate::security::apply(&crate::prefs::get(cx));
                        cx.notify();
                    }))
                })
            }),
            l,
        );
        let passwords = seg_group(
            [(true, "settings-passwords-vault"), (false, "settings-passwords-none")].map(|(v, key)| {
                let enabled = policy.store_password.is_none();
                seg(("settings-passwords", v as usize), tr!(key), eff.store_password == v, enabled, l).when(enabled, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        crate::prefs::update(cx, |p| p.security.store_password = v);
                        // «Не хранить» — сохранённый пароль удаляется сразу.
                        if !v && this.stored == Some(true) {
                            this.wipe_sign_ins(cx);
                        }
                        cx.notify();
                    }))
                })
            }),
            l,
        );
        // Проверка отзыва — всегда: выключатель закреплён включённым.
        let revocation = div().opacity(0.6).child(crate::settings_tab::switch(true, l));
        // Песочница: уровень последнего разбора (экран почты); ещё не разбирали — «проверяется при первом письме».
        let (sandbox, sandbox_hint) = match self.info.sandbox {
            Some(Level::Full) => (tr!("settings-sandbox-full"), tr!("settings-sandbox-full-hint")),
            Some(level) => (crate::reading::sandbox_label(level), tr!("settings-sandbox-partial-hint")),
            None => (tr!("settings-sandbox-unknown"), tr!("settings-sandbox-unknown-hint")),
        };
        let partial = matches!(self.info.sandbox, Some(l) if l != Level::Full);
        let chip = div()
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_md()
            .when(!partial, |d| d.bg(l.fill()).text_color(l.text()))
            .when(partial, |d| d.bg(l.error_bg()).text_color(l.error()))
            .child(sandbox);
        let help = self.sandbox_help;
        let stored = self.stored == Some(true);
        let wipe = if self.wipe_confirm {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(button("settings-wipe-yes", tr!("settings-wipe-yes"), true, l).on_click(cx.listener(|this, _, _, cx| this.wipe_sign_ins(cx))))
                .child(link("settings-wipe-no", tr!("settings-cancel"), l).on_click(cx.listener(|this, _, _, cx| {
                    this.wipe_confirm = false;
                    cx.notify();
                })))
                .into_any_element()
        } else if stored {
            button("settings-wipe", tr!("settings-wipe"), false, l)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.wipe_confirm = true;
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let wipe_hint = if self.wiped {
            tr!("settings-wipe-done")
        } else if self.wipe_confirm {
            tr!("settings-wipe-ask")
        } else if stored {
            tr!("settings-wipe-hint")
        } else {
            tr!("settings-wipe-nothing")
        };
        div()
            .flex()
            .flex_col()
            .when_some(self.os.error.clone(), |d, e| d.child(div().pt_3().text_color(l.error()).child(e)))
            .child(group(tr!("settings-content-group"), l))
            .child(row(
                tr!("settings-images"),
                Some(pinned(
                    tr!(if eff.images == Images::Ask { "settings-images-ask-hint" } else { "settings-images-hint" }),
                    policy.images.is_some(),
                )),
                images,
                l,
            ))
            .child(row(
                tr!("settings-link-mismatch"),
                Some(tr!("settings-link-mismatch-hint")),
                toggle("settings-sec-link", p.reading.warn_link_mismatch, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.reading.warn_link_mismatch = !p.reading.warn_link_mismatch);
                    cx.notify();
                })),
                l,
            ))
            .child(row(
                tr!("settings-browser"),
                Some(tr!("settings-browser-hint")),
                div().text_color(l.text2()).child(tr!("settings-browser-system")),
                l,
            ))
            .child(group(tr!("settings-attach-group"), l))
            .child(row(tr!("settings-macros"), Some(tr!("settings-macros-hint")), fixed("settings-macros-block", "settings-macros-blocked"), l))
            .child(row(
                tr!("settings-macro-save"),
                Some(tr!("settings-macro-save-hint")),
                toggle("settings-macro-save", p.security.macro_save, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.security.macro_save = !p.security.macro_save);
                    cx.notify();
                })),
                l,
            ))
            .child(row(
                tr!("settings-sign-all"),
                Some(tr!("settings-sign-all-hint")),
                toggle("settings-sign-all", p.security.sign_all, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.security.sign_all = !p.security.sign_all);
                    cx.notify();
                })),
                l,
            ))
            .child(group(tr!("settings-sandbox-group"), l))
            .child(row(
                tr!("settings-sandbox"),
                Some(sandbox_hint),
                div().flex().items_center().gap_2().child(chip).when(partial, |d| {
                    d.child(
                        link("settings-sandbox-help", tr!(if help { "settings-sandbox-help-hide" } else { "settings-sandbox-help" }), l).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.sandbox_help = !this.sandbox_help;
                                cx.notify();
                            }),
                        ),
                    )
                }),
                l,
            ))
            .when(partial && help, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_3()
                        .mt_2()
                        .rounded_md()
                        .bg(l.fill_passive())
                        .text_size(px(12.))
                        .child(tr!("settings-sandbox-howto"))
                        .child(div().font_family("Consolas").text_color(l.text()).child(crate::diagnostics::icacls_command()))
                        .child(div().text_color(l.text2()).child(tr!("settings-sandbox-howto-after"))),
                )
            })
            .child(group(tr!("settings-phishing-group"), l))
            .child(row(
                tr!("settings-phishing"),
                Some(if self.phishing_error { tr!("settings-phishing-bad") } else { tr!("settings-phishing-hint") }),
                div().w(px(280.)).child(gpui_kit::component::input::Input::new(&self.phishing)),
                l,
            ))
            .child(group(tr!("settings-signin-group"), l))
            .child(row(
                tr!("settings-passwords"),
                Some(pinned(
                    tr!(if eff.store_password { "settings-passwords-hint" } else { "settings-passwords-none-hint" }),
                    policy.store_password.is_some(),
                )),
                passwords,
                l,
            ))
            .child(row(
                tr!("settings-tls"),
                Some(pinned(tr!(if eff.tls13 { "settings-tls-13-hint" } else { "settings-tls-hint" }), policy.tls13.is_some())),
                tls,
                l,
            ))
            .child(row(tr!("settings-revocation"), Some(tr!("settings-revocation-hint")), revocation, l))
            .child(row(tr!("settings-wipe-title"), Some(wipe_hint), wipe, l))
    }
}
