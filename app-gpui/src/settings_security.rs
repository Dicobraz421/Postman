//! «Настройки → Безопасность» (S5-30, макет `SettingsSecurity`): содержимое писем, вложения, песочница, фишинг, входы
//! и соединение. Выбор — только строже прежнего (решение владельца 2026-10-22): картинки «никогда» или «спрашивать»
//! (с доверенными доменами, T3-04 — `remote_images.rs`),
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
                        tracing::error!("security: passwords not removed from the credential store: {e}");
                        v.os.error = Some(tr!("settings-os-failed"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Диск с данными Postman: BitLocker — в фоне (UI-поток не ждёт); демо — «не зашифрован» (снимок).
    fn check_disk(&mut self, cx: &mut Context<Self>) {
        if self.disk_asked {
            return;
        }
        self.disk_asked = true;
        if !self.info.live {
            let demo = if cfg!(target_os = "macos") { "Macintosh HD" } else { "C:" };
            self.disk = Some((demo.into(), Some(false)));
            return;
        }
        let task = cx.background_spawn(async move { crate::sys::shell::bitlocker(&crate::data_dir()) });
        cx.spawn(async move |this, cx| {
            let disk = task.await;
            let _ = this.update(cx, |v, cx| {
                v.disk = Some(disk);
                cx.notify();
            });
        })
        .detach();
    }

    /// Свои сертификаты S/MIME (S5-12.7): хранилище ОС — в фоне (UI-поток не ждёт); демо — сертификат демо-ящика.
    fn check_certs(&mut self, cx: &mut Context<Self>) {
        if self.certs_asked {
            return;
        }
        self.certs_asked = true;
        if !self.info.live {
            self.certs = Some(demo_certs());
            return;
        }
        let task = cx.background_spawn(async move { postman_core::smime::personal_certificates() });
        cx.spawn(async move |this, cx| {
            let certs = task.await;
            let _ = this.update(cx, |v, cx| {
                v.certs = Some(certs);
                cx.notify();
            });
        })
        .detach();
    }

    /// Группа «Свои сертификаты»: по строке на сертификат — кому выдан; адреса, издатель, срок; годен ли для подписи.
    /// Истекает через ≤ 30 дней — срок цветом сроков; истёк — серым.
    fn cert_rows(&self, l: &Look) -> Div {
        let mut d = div().flex().flex_col().child(group(tr!("settings-certs-group"), l));
        let Some(certs) = &self.certs else {
            return d.child(row(tr!("settings-certs-loading"), None, div(), l));
        };
        if certs.is_empty() {
            return d.child(row(tr!("settings-certs-none"), Some(tr!("settings-certs-none-hint")), div(), l));
        }
        let now = jiff::Timestamp::now().as_second();
        for c in certs {
            let (state, tone) = cert_state(c, now);
            let control = div()
                .flex()
                .flex_col()
                .items_end()
                .gap_0p5()
                .child(div().text_color(l.text2()).child(tr!(if c.can_sign { "settings-cert-sign" } else { "settings-cert-no-sign" })))
                .when_some(state, |d, s| {
                    d.child(
                        div()
                            .text_color(match tone {
                                CertTone::Soon => l.warning(),
                                CertTone::Plain => l.text2(),
                            })
                            .child(s),
                    )
                });
            d = d.child(row(c.info.subject.clone(), Some(cert_hint(c)), control, l));
        }
        d
    }

    /// Первая строка «Безопасности» (S8-35, макет S8-D): шифрование диска с базой — одно из трёх состояний,
    /// нейтрально, без баннеров и тостов.
    fn disk_row(&self, l: &Look, cx: &mut Context<Self>) -> Option<Div> {
        let (drive, state) = self.disk.clone()?;
        let (glyph, title, hint) = disk_texts(&drive, state);
        let button = (state == Some(false)).then(|| {
            button("settings-bitlocker-open", tr!("settings-bitlocker-open"), false, l).on_click(cx.listener(|_, _, _, cx| {
                if let Err(e) = crate::sys::shell::open_bitlocker() {
                    tracing::error!("security: disk encryption settings not opened: {e}");
                }
                cx.notify();
            }))
        });
        Some(
            div()
                .flex()
                .items_center()
                .gap_3()
                .py_3()
                .border_b_1()
                .border_color(l.border())
                .child(crate::chrome::icon(glyph, 16.).text_color(l.text2()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(title)
                        .children(hint.map(|h| div().text_size(px(12.)).text_color(l.text2()).child(h))),
                )
                .children(button),
        )
    }

    /// «Состояние защиты» (T2-57, макет `SettingsSecurity`): песочница, макросы, пароли, соединение, фишинг, диск.
    pub(crate) fn protection_card(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        self.check_disk(cx);
        let p = crate::prefs::get(cx);
        let eff = crate::security::effective(&p);
        let full = self.info.sandbox == Some(Level::Full);
        let line = |ok: Option<bool>, title: String, value: String| {
            let (g, color) = match ok {
                Some(true) => (crate::chrome::glyph::CHECK, l.accent_text()),
                Some(false) => (crate::chrome::glyph::WARNING, l.warning()),
                None => (crate::chrome::glyph::INFO, l.text3()),
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(crate::chrome::icon(g, 12.).text_color(color))
                .child(div().flex_1().child(title))
                .child(div().text_color(if ok == Some(false) { l.warning() } else { l.text() }).child(value))
        };
        let sandbox = match self.info.sandbox {
            Some(Level::Full) => tr!("settings-protection-full"),
            Some(_) => tr!("settings-protection-partial"),
            None => tr!("settings-protection-unknown"),
        };
        let phishing = p.general.phishing_report.clone();
        let disk = self.disk.clone().map(|(drive, state)| {
            let v = match state {
                Some(true) => tr!("settings-protection-disk-on", drive = drive),
                Some(false) => tr!("settings-protection-disk-off", drive = drive),
                None => tr!("settings-protection-unknown"),
            };
            line(state, tr!("settings-protection-disk"), v)
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_4()
            .rounded(l.px(pets_ui::theme::Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.panel())
            .child(div().pb_1().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(tr!("settings-protection")))
            .child(line(self.info.sandbox.map(|_| full), tr!("settings-protection-sandbox"), sandbox))
            .child(line(Some(true), tr!("settings-protection-macros"), tr!("settings-protection-macros-on")))
            .child(line(
                None,
                tr!("settings-protection-passwords"),
                tr!(if eff.store_password { "settings-protection-vault" } else { "settings-protection-no-store" }),
            ))
            .child(line(None, tr!("settings-protection-tls"), tr!(if eff.tls13 { "settings-protection-tls13" } else { "settings-protection-tls12" })))
            .child(line(
                Some(phishing.is_some()),
                tr!("settings-protection-phishing"),
                phishing.unwrap_or_else(|| tr!("settings-protection-no-address")),
            ))
            .children(disk)
    }

    pub(crate) fn security(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        self.check_disk(cx);
        self.check_certs(cx);
        let disk = self.disk_row(l, cx);
        let certs = self.cert_rows(l);
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
        let directory = self.directory_rows(l, cx);
        let domains = self.image_domains(l, cx);
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
            .children(disk)
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
            // Доверенные домены картинок (T3-04).
            .child(domains)
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
            .child(certs)
            .child(group(tr!("settings-sandbox-group"), l))
            .child(row(
                tr!("settings-sandbox"),
                Some(sandbox_hint),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(chip)
                    // «Проверить снова» (T2-57): права на папку выдали — полная изоляция без перезапуска.
                    .child(if self.sandbox_checking {
                        div().text_color(l.text2()).child(tr!("settings-sandbox-checking")).into_any_element()
                    } else {
                        link("settings-sandbox-recheck", tr!("settings-sandbox-recheck"), l)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sandbox_checking = true;
                                cx.emit(crate::settings_tab::SettingsEvent::RecheckSandbox);
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .when(partial, |d| {
                        d.child(
                            link("settings-sandbox-help", tr!(if help { "settings-sandbox-help-hide" } else { "settings-sandbox-help" }), l)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sandbox_help = !this.sandbox_help;
                                    cx.notify();
                                })),
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
                        .child(div().font_family(crate::chrome::MONO_FONT).text_color(l.text()).child(crate::diagnostics::icacls_command()))
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
            .child(directory)
    }
}

/// За сколько до конца срока предупреждать (`docs/SMIME.md` §7).
const CERT_SOON: i64 = 30 * 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CertTone {
    Plain,
    Soon,
}

/// Строка состояния сертификата: истёк, истекает скоро, ключ не поддерживается; иначе — ничего.
fn cert_state(c: &postman_core::smime::Personal, now: i64) -> (Option<String>, CertTone) {
    let until = date(c.info.not_after);
    if c.info.not_after < now {
        (Some(tr!("settings-cert-expired", date = until)), CertTone::Plain)
    } else if c.info.not_after - now <= CERT_SOON {
        (Some(tr!("settings-cert-expiring", date = until)), CertTone::Soon)
    } else if c.scheme.is_none() {
        (Some(tr!("settings-cert-unsupported")), CertTone::Plain)
    } else {
        (None, CertTone::Plain)
    }
}

/// Пояснение: адреса, издатель, срок.
fn cert_hint(c: &postman_core::smime::Personal) -> String {
    let emails = if c.info.emails.is_empty() { tr!("settings-cert-no-address") } else { c.info.emails.join(", ") };
    tr!("settings-cert-hint", emails = emails, issuer = c.info.issuer.as_str(), until = date(c.info.not_after))
}

fn date(ts: i64) -> String {
    jiff::Timestamp::from_second(ts).map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%d.%m.%Y").to_string()).unwrap_or_default()
}

/// Демо-ящик: свой сертификат демо (Смирнова Анна, `me@corp.example`) — для снимка.
fn demo_certs() -> Vec<postman_core::smime::Personal> {
    let der = postman_demo::smime::my_key().0.to_vec();
    postman_smime::sign::own_certificate(&der)
        .map(|(info, can_sign, scheme)| vec![postman_core::smime::Personal { der, info, can_sign, scheme }])
        .unwrap_or_default()
}

/// Значок, заголовок и пояснение строки диска: не зашифрован — предупреждение и «Параметры BitLocker»; зашифрован —
/// галочка; неизвестно — сведения.
pub(crate) fn disk_texts(drive: &str, state: Option<bool>) -> (char, String, Option<String>) {
    use crate::chrome::glyph;
    match state {
        Some(false) => (glyph::WARNING, tr!("settings-bitlocker-off", drive = drive), Some(tr!("settings-bitlocker-off-hint"))),
        Some(true) => (glyph::CHECK, tr!("settings-bitlocker-on", drive = drive), None),
        None => (glyph::INFO, tr!("settings-bitlocker-unknown", drive = drive), Some(tr!("settings-bitlocker-unknown-hint"))),
    }
}

#[cfg(test)]
mod tests {
    /// Три состояния диска (S8-35) — свои значок и тексты на обоих языках.
    #[test]
    fn disk_states() {
        use crate::chrome::glyph;
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        let (g, t, h) = super::disk_texts("C:", Some(false));
        assert_eq!((g, t.as_str()), (glyph::WARNING, "Диск C: с данными Postman не зашифрован"));
        assert!(h.is_some());
        assert_eq!(super::disk_texts("C:", Some(true)).0, glyph::CHECK);
        assert!(super::disk_texts("C:", Some(true)).2.is_none());
        assert_eq!(super::disk_texts("D:", None).0, glyph::INFO);
        let _ = pets_i18n::init(&crate::parts::texts(), "en");
        assert_eq!(super::disk_texts("C:", Some(true)).1, "Drive C: with Postman data is encrypted");
    }

    /// S5-12.7: свой сертификат демо — годен для подписи, без строки состояния; истекает через 10 дней — «Истекает»
    /// цветом сроков; истёк — «Истёк»; в пояснении — адрес, издатель, срок.
    #[test]
    fn cert_states() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        let certs = super::demo_certs();
        assert_eq!(certs.len(), 1);
        let c = &certs[0];
        assert!(c.can_sign);
        let until = c.info.not_after;
        assert_eq!(super::cert_state(c, until - 365 * 24 * 3600), (None, super::CertTone::Plain));
        let (soon, tone) = super::cert_state(c, until - 10 * 24 * 3600);
        assert!(soon.is_some_and(|s| s.starts_with("Истекает")) && tone == super::CertTone::Soon);
        let (gone, tone) = super::cert_state(c, until + 1);
        assert!(gone.is_some_and(|s| s.starts_with("Истёк ")) && tone == super::CertTone::Plain);
        let hint = super::cert_hint(c);
        assert!(hint.contains("me@corp.example") && hint.contains("действует до"), "{hint}");
    }
}
