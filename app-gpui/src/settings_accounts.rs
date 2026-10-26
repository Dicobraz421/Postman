//! «Настройки → Аккаунты» (S5-09, `Account.dc.html`): учётная запись (сервер, логин, состояние синхронизации, где
//! пароль), общие ящики, безопасность подключения; «Добавить аккаунт» — карточка на месте (`signin.rs`), «Удалить» —
//! подтверждением строкой. Учётных записей может быть несколько (S7-03б): первая — основная, «Добавить аккаунт» при
//! живой учётной записи добавляет ещё одну рядом; каждая — группой в дереве папок, как общие ящики.

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsEvent, SettingsView, aside_card, link};

/// Инициалы для кружка: первые буквы имени до `@` (две), иначе первая буква.
pub(crate) fn initials(user: &str) -> String {
    let name = user.split('@').next().unwrap_or(user);
    let parts: Vec<&str> = name.split(['.', '_', '-', ' ']).filter(|p| !p.is_empty()).collect();
    let first = |s: &str| s.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    match parts.as_slice() {
        [a, b, ..] => format!("{}{}", first(a), first(b)),
        [a] => first(a),
        [] => "?".into(),
    }
}

/// Имя узла из адреса сервера.
fn host(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, r)| r).split(['/', '?']).next().unwrap_or(url)
}

impl SettingsView {
    pub(crate) fn accounts(&mut self, l: &Look, cx: &mut Context<Self>) -> (Option<Stateful<Div>>, AnyElement, Vec<Div>) {
        let adding = self.add.is_some();
        let add = (!adding).then(|| {
            button("settings-account-add", format!("+ {}", tr!("settings-account-add")), false, l).on_click(cx.listener(|this, _, _, cx| {
                cx.emit(SettingsEvent::AddAccount);
                this.remove_confirm = false;
                cx.notify();
            }))
        });
        let mut list = div().flex().flex_col().gap_2().pt_3();
        let avatar = |text: String| {
            div()
                .size(px(36.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(l.picked())
                .text_color(l.accent_text())
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let card = || div().flex().items_center().gap_3().p_3().rounded(l.px(Metric::RadiusOverlay)).border_1().border_color(l.border());
        match (&self.info.account, self.info.live) {
            (Some(a), true) => {
                let password = match self.stored {
                    Some(true) => tr!("settings-account-password-stored"),
                    Some(false) => tr!("settings-account-password-none"),
                    None => String::new(),
                };
                let status = if self.info.status.is_empty() { tr!("settings-account-idle") } else { self.info.status.clone() };
                let confirm = self.remove_confirm;
                list = list.child(
                    card()
                        .child(avatar(initials(&a.user)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(a.user.clone()))
                                .child(div().text_size(px(12.)).text_color(l.text2()).truncate().child(tr!(
                                    "settings-account-line",
                                    host = host(&a.url).to_owned(),
                                    status = status
                                )))
                                .when(!password.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(l.text2()).child(password))),
                        )
                        .when(!confirm, |d| {
                            d.child(link("settings-account-remove", tr!("settings-account-remove"), l).on_click(cx.listener(|this, _, _, cx| {
                                this.remove_confirm = true;
                                cx.notify();
                            })))
                        }),
                );
                if confirm {
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .p_2()
                            .rounded(l.px(Metric::RadiusControl))
                            .bg(l.warning_bg())
                            .child(div().flex_1().child(tr!("settings-account-remove-confirm", user = a.user.clone())))
                            .child(button("settings-account-remove-yes", tr!("settings-account-remove"), true, l).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.remove_confirm = false;
                                    cx.emit(SettingsEvent::RemoveAccount);
                                },
                            )))
                            .child(link("settings-account-remove-no", tr!("settings-cancel"), l).on_click(cx.listener(|this, _, _, cx| {
                                this.remove_confirm = false;
                                cx.notify();
                            }))),
                    );
                }
            }
            _ => {
                list = list.child(
                    card().child(avatar("D".into())).child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("settings-account-demo")))
                            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-account-demo-hint"))),
                    ),
                );
            }
        }
        let shared_row = |name: &String| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .pl(px(48.))
                .py_1()
                .child(crate::chrome::icon(crate::chrome::glyph::FOLDER, 14.).text_color(l.text2()))
                .child(div().flex_1().child(name.clone()))
                .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-account-shared")))
        };
        for name in &self.info.shared {
            list = list.child(shared_row(name));
        }
        // Дополнительные учётные записи (S7-03б): та же карточка, «Удалить» — подтверждением строкой.
        for (n, (profile, a, shared)) in self.info.extra.iter().enumerate() {
            let confirm = self.remove_extra.as_deref() == Some(profile.as_str());
            let ask = profile.clone();
            list = list.child(
                card()
                    .child(avatar(initials(&a.user)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(a.user.clone()))
                            .child(div().text_size(px(12.)).text_color(l.text2()).truncate().child(host(&a.url).to_owned())),
                    )
                    .when(!confirm, |d| {
                        d.child(link(("settings-extra-remove", n), tr!("settings-account-remove"), l).on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_extra = Some(ask.clone());
                            cx.notify();
                        })))
                    }),
            );
            if confirm {
                let gone = profile.clone();
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_2()
                        .rounded(l.px(Metric::RadiusControl))
                        .bg(l.warning_bg())
                        .child(div().flex_1().child(tr!("settings-account-remove-confirm", user = a.user.clone())))
                        .child(button(("settings-extra-remove-yes", n), tr!("settings-account-remove"), true, l).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.remove_extra = None;
                                cx.emit(SettingsEvent::RemoveExtra(gone.clone()));
                            },
                        )))
                        .child(link(("settings-extra-remove-no", n), tr!("settings-cancel"), l).on_click(cx.listener(|this, _, _, cx| {
                            this.remove_extra = None;
                            cx.notify();
                        }))),
                );
            }
            for name in shared {
                list = list.child(shared_row(name));
            }
        }
        let body = div()
            .flex()
            .flex_col()
            .child(list)
            .when_some(self.add.clone(), |d, v| d.child(div().pt_3().child(v)))
            .child(div().pt_3().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-account-one")));
        let (tls, ok) = crate::signin::security_line();
        let security = aside_card(tr!("settings-account-security"), vec![tr!("settings-account-tls"), tr!("settings-account-revocation")], l)
            .when(!ok, |d| d.child(div().text_color(l.warning()).child(tls)));
        let secrets = aside_card(tr!("settings-account-secrets"), vec![tr!("settings-account-secrets-1"), tr!("settings-account-secrets-2")], l);
        (add, body.into_any_element(), vec![security, secrets])
    }
}

#[cfg(test)]
mod tests {
    use super::{host, initials};

    #[test]
    fn account_initials_and_host() {
        assert_eq!(initials("anna.petrova@corp.example"), "AP");
        assert_eq!(initials("user1@itt.test"), "U");
        assert_eq!(initials(""), "?");
        assert_eq!(host("https://mail.example.com:8443/jmap"), "mail.example.com:8443");
        assert_eq!(host("mail.example.com"), "mail.example.com");
    }
}
