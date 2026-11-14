//! «Настройки → Аккаунты» (S5-09, `Account.dc.html`): учётная запись (сервер, логин, состояние синхронизации, где
//! пароль), общие ящики, безопасность подключения; «Добавить аккаунт» — карточка на месте (`signin.rs`), «Удалить» —
//! подтверждением строкой. Учётных записей может быть несколько (S7-03б): первая — основная, «Добавить аккаунт» при
//! живой учётной записи добавляет ещё одну рядом; каждая — группой в дереве папок, как общие ящики. Демо — три
//! учётки `postman_demo::ACCOUNTS` (S7-09б) той же карточкой: адрес, узел, вид сервера.

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

/// Подпись вида сервера учётки демо: ключ варианта `settings-account-kind`.
fn kind(k: postman_demo::Kind) -> &'static str {
    match k {
        postman_demo::Kind::Jmap => "jmap",
        postman_demo::Kind::Standards => "standards",
        postman_demo::Kind::Exchange => "exchange",
    }
}

/// Карточка учётной записи демо — как карточка дополнительной учётки: кружок, адрес, узел и вид сервера; удалять
/// нечего.
fn demo_card(a: &postman_demo::Account, l: &Look) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .rounded(l.px(Metric::RadiusOverlay))
        .border_1()
        .border_color(l.border())
        .child(avatar(initials(a.email), l))
        .child(div().flex_1().min_w_0().flex().flex_col().gap_0p5().child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(a.email)).child(
            div().text_size(px(12.)).text_color(l.text2()).truncate().child(tr!(
                "settings-account-line",
                host = a.host.to_owned(),
                status = tr!("settings-account-kind", kind = kind(a.kind))
            )),
        ))
}

/// Адрес пути по частям: (протокол, узел, порт, путь, шифрование) — «Параметры сервера» (T2-51).
pub(crate) fn server_parts(url: &str) -> (String, String, String, String, bool) {
    let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
    let (hostport, path) = rest.split_once('/').map_or((rest, String::new()), |(h, p)| (h, format!("/{p}")));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h.to_owned(), p.to_owned()),
        _ => {
            let port = match scheme {
                "imaps" => "993",
                "imap" => "143",
                "http" => "80",
                _ => "443",
            };
            (hostport.to_owned(), port.to_owned())
        }
    };
    let tls = matches!(scheme, "https" | "imaps");
    (scheme.to_owned(), host, port, path, tls)
}

/// Строка «Название — значение» карточки «Настроить».
fn param(name: String, value: String, l: &Look) -> Div {
    div()
        .flex()
        .gap_3()
        .py_1()
        .child(div().w(px(160.)).flex_none().text_color(l.text2()).child(name))
        .child(div().flex_1().min_w_0().truncate().child(value))
}

/// Кружок с инициалами учётной записи.
fn avatar(text: String, l: &Look) -> Div {
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
}

impl SettingsView {
    /// Размер кэша учётной записи на диске — в фоне.
    pub(crate) fn read_cache_size(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.info.cache.clone() else { return };
        let task = cx.background_spawn(async move { std::fs::metadata(&path).map(|m| m.len()).ok() });
        cx.spawn(async move |this, cx| {
            let size = task.await;
            let _ = this.update(cx, |v, cx| {
                v.cache_size = size.or(Some(0));
                cx.notify();
            });
        })
        .detach();
    }

    /// «Настроить»: параметры сервера и синхронизация учётной записи.
    fn account_details(&self, url: &str, user: &str, kind: String, l: &Look) -> Div {
        let (_, host, port, path, tls) = server_parts(url);
        let cache = match (&self.info.cache, self.cache_size) {
            (None, _) => tr!("settings-account-cache-demo"),
            (Some(_), None) => tr!("settings-account-cache-reading"),
            (Some(_), Some(n)) => crate::bulk::size_text(n),
        };
        div()
            .ml(px(48.))
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .rounded(l.px(Metric::RadiusControl))
            .bg(l.fill_passive())
            .border_1()
            .border_color(l.border())
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr!("settings-account-server")))
            .child(param(tr!("settings-account-protocol"), kind, l))
            .child(param(tr!("settings-account-host"), host, l))
            .child(param(tr!("settings-account-port"), port, l))
            .when(!path.is_empty(), |d| d.child(param(tr!("settings-account-path"), path, l)))
            .child(param(tr!("settings-account-tls-row"), if tls { tr!("settings-account-tls-on") } else { tr!("settings-account-tls-off") }, l))
            .when(!user.is_empty(), |d| d.child(param(tr!("settings-account-login"), user.to_owned(), l)))
            .child(div().pt_2().font_weight(FontWeight::SEMIBOLD).child(tr!("settings-account-sync")))
            .child(param(tr!("settings-account-cache"), cache, l))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-account-sync-note")))
    }

    /// «Настроить» и «Синхронизировать» в карточке учётной записи.
    fn account_actions(&self, key: &str, cx: &mut Context<Self>, l: &Look) -> Div {
        let open = self.account_open.as_deref() == Some(key);
        let k = key.to_owned();
        let n = if key == "main" { 0 } else { 1 + key.len() };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(link(("settings-account-setup", n), tr!(if open { "settings-account-setup-hide" } else { "settings-account-setup" }), l).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.account_open = if this.account_open.as_deref() == Some(k.as_str()) { None } else { Some(k.clone()) };
                    if this.account_open.is_some() {
                        this.read_cache_size(cx);
                    }
                    cx.notify();
                }),
            ))
            .child(link(("settings-account-sync-now", n), tr!("settings-account-sync-now"), l).on_click(cx.listener(|this, _, _, cx| {
                this.syncing = true;
                cx.emit(SettingsEvent::SyncNow);
                cx.notify();
            })))
    }

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
        let avatar = |text: String| avatar(text, l);
        let card = || div().flex().items_center().gap_3().p_3().rounded(l.px(Metric::RadiusOverlay)).border_1().border_color(l.border());
        let demo = !(self.info.live && self.info.account.is_some());
        match (&self.info.account, self.info.live) {
            (Some(a), true) => {
                let password = match self.stored {
                    // Вход учётной записью Windows (T1-02б): пароля нет.
                    _ if a.windows => tr!("settings-account-windows"),
                    Some(true) => tr!("settings-account-password-stored"),
                    Some(false) => tr!("settings-account-password-none"),
                    None => String::new(),
                };
                let status = if self.syncing {
                    tr!("settings-account-syncing")
                } else if self.info.status.is_empty() {
                    tr!("settings-account-idle")
                } else {
                    self.info.status.clone()
                };
                // Протокол в строке (T2-51): «Exchange (EWS) · mail.corp · синхронизирован».
                let (kind, _) = crate::signin::kind_of(&a.url);
                let status = format!("{kind} · {status}");
                let confirm = self.remove_confirm;
                let actions = self.account_actions("main", cx, l);
                let details = (self.account_open.as_deref() == Some("main")).then(|| self.account_details(&a.url, &a.user, kind.clone(), l));
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
                            d.child(actions).child(link("settings-account-remove", tr!("settings-account-remove"), l).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.remove_confirm = true;
                                    cx.notify();
                                },
                            )))
                        }),
                );
                list = list.children(details);
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
            // Демо (S7-09б): основная учётка демо-ящика; дополнительные — ниже, после её общих ящиков.
            _ => {
                if let Some(a) = postman_demo::ACCOUNTS.first() {
                    let actions = self.account_actions("main", cx, l);
                    list = list.child(demo_card(a, l).child(actions));
                    if self.account_open.as_deref() == Some("main") {
                        let url = format!("https://{}/jmap", a.host);
                        list = list.child(self.account_details(&url, a.email, tr!("settings-account-kind", kind = kind(a.kind)), l));
                    }
                }
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
        // Дополнительные учётные записи (S7-03б): та же карточка, «Удалить» — подтверждением строкой. Демо — свои
        // карточки ниже (учётки демо-ящика в кэше — те же `ACCOUNTS`, но без узла и вида сервера).
        let extra = if demo { &[][..] } else { &self.info.extra[..] };
        for (n, (profile, a, shared)) in extra.iter().enumerate() {
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
        if demo {
            for a in postman_demo::ACCOUNTS.iter().skip(1) {
                list = list.child(demo_card(a, l));
            }
        }
        let body = div()
            .flex()
            .flex_col()
            .child(list)
            .when_some(self.add.clone(), |d, v| d.child(div().pt_3().child(v)))
            .child(div().pt_3().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-account-one")))
            .child(self.channels.clone());
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
