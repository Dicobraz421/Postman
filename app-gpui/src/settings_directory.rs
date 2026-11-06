//! «Настройки → Безопасность → Каталог сертификатов» (S5-12.5б, решение владельца №12): сервер, база поиска, вход,
//! StartTLS и «Проверить». Пустой сервер — контроллер домена компьютера (подсказка показывает, какой нашёлся), пустая
//! база — из rootDSE. Пароль — в Диспетчере учётных данных (`directory::credential_target`), пишется по Enter или
//! при уходе из поля, на диск настроек не попадает.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_core::prefs::DirectoryLogin;

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsView, group, row, seg, seg_group, toggle};

/// Поля каталога и итог «Проверить».
pub(crate) struct DirState {
    server: Entity<InputState>,
    base: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    /// Контроллер домена компьютера: `None` — ещё не спрашивали, `Some(None)` — не в домене.
    dc: Option<Option<String>>,
    dc_asked: bool,
    /// «Проверить»: идёт (`None` внутри) или итог.
    check: Option<Option<String>>,
    /// Пароль сохранён (подсказка поля).
    saved: bool,
}

impl DirState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<SettingsView>, subscriptions: &mut Vec<Subscription>) -> Self {
        let p = crate::prefs::get(cx).security.directory;
        let input = |value: String, placeholder: String, masked: bool, window: &mut Window, cx: &mut Context<SettingsView>| {
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder).masked(masked);
                s.set_value(value, window, cx);
                s
            })
        };
        let server = input(p.server.clone(), tr!("settings-directory-server-auto"), false, window, cx);
        let base = input(p.base_dn.clone(), tr!("settings-directory-base-auto"), false, window, cx);
        let user = input(p.user.clone(), tr!("settings-directory-user-placeholder"), false, window, cx);
        let password = input(String::new(), tr!("settings-directory-password-placeholder"), true, window, cx);
        // Сервер, база, имя — в настройки по мере ввода; пароль — по Enter или при уходе из поля.
        for (state, field) in [(&server, Field::Server), (&base, Field::Base), (&user, Field::User)] {
            subscriptions.push(cx.subscribe_in(state, window, move |this: &mut SettingsView, state, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    let text = state.read(cx).value().trim().to_owned();
                    crate::prefs::update(cx, |p| {
                        let d = &mut p.security.directory;
                        match field {
                            Field::Server => d.server = text,
                            Field::Base => d.base_dn = text,
                            Field::User => d.user = text,
                        }
                    });
                    this.dir.check = None;
                    cx.notify();
                }
            }));
        }
        subscriptions.push(cx.subscribe_in(&password, window, |this: &mut SettingsView, state, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                let secret = state.read(cx).value().to_string();
                if !secret.is_empty() {
                    state.update(cx, |s, cx| s.set_value("", window, cx));
                    this.save_directory_password(secret, cx);
                }
            }
        }));
        Self { server, base, user, password, dc: None, dc_asked: false, check: None, saved: false }
    }
}

#[derive(Clone, Copy)]
enum Field {
    Server,
    Base,
    User,
}

impl SettingsView {
    /// Пароль каталога — в Диспетчер учётных данных (в фоне), поле очищается.
    fn save_directory_password(&mut self, secret: String, cx: &mut Context<Self>) {
        let p = crate::prefs::get(cx).security.directory;
        let target = crate::directory::credential_target(&p.server);
        let task = cx.background_spawn(async move { crate::sys::credentials::write(&target, p.user.trim(), &secret) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(()) => v.dir.saved = true,
                    Err(e) => {
                        tracing::warn!("credential manager: {e}");
                        v.os.error = Some(tr!("settings-os-failed"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Контроллер домена — в фоне, один раз за открытие настроек; демо — «не в домене». Сервер указан — подсказке он не нужен.
    fn ask_domain_controller(&mut self, server: &str, cx: &mut Context<Self>) {
        if self.dir.dc_asked || !server.is_empty() {
            return;
        }
        self.dir.dc_asked = true;
        if !self.info.live {
            self.dir.dc = Some(None);
            return;
        }
        let task = cx.background_spawn(async move { postman_backend_ldap::domain_controller() });
        cx.spawn(async move |this, cx| {
            let dc = task.await;
            let _ = this.update(cx, |v, cx| {
                v.dir.dc = Some(dc);
                cx.notify();
            });
        })
        .detach();
    }

    /// «Проверить»: спросить каталог о своём адресе; итог — подсказкой строки. Сертификаты не запоминаются: это
    /// проверка связи и входа, проверенные в кэш кладёт только поиск из письма.
    fn check_directory(&mut self, cx: &mut Context<Self>) {
        self.dir.check = Some(None);
        let live = self.info.live;
        let me = self.info.account.as_ref().map(|a| a.user.clone()).unwrap_or_default();
        let prefs = crate::prefs::get(cx).security.directory;
        let task = cx.background_spawn(async move {
            let dir: std::sync::Arc<dyn postman_core::smime::directory::Directory> =
                if live { crate::directory::from_prefs(&prefs) } else { std::sync::Arc::new(crate::directory::Empty) };
            postman_core::smime::directory::lookup(dir, &[me], postman_core::smime::directory::TIMEOUT)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                v.dir.check = Some(Some(match result {
                    Ok(list) => tr!("settings-directory-ok", n = list.len()),
                    Err(e) => crate::directory::error_text(&e),
                }));
                cx.notify();
            });
        })
        .detach();
    }

    /// Итог «Проверить»: `None` — ещё идёт или не нажимали.
    pub(crate) fn directory_check(&self) -> Option<&str> {
        self.dir.check.as_ref().and_then(|c| c.as_deref())
    }

    /// Подсказка поля пароля: «Сохранён в Диспетчере учётных данных»; в поле пароля не осталось.
    pub(crate) fn directory_password_saved(&self, cx: &App) -> bool {
        self.dir.saved && self.dir.password.read(cx).value().is_empty()
    }

    /// Группа «Каталог сертификатов».
    pub(crate) fn directory_rows(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = crate::prefs::get(cx).security.directory;
        self.ask_domain_controller(&p.server, cx);
        let server_hint = match &self.dir.dc {
            _ if !p.server.is_empty() => tr!("settings-directory-server-hint"),
            Some(Some(dc)) => tr!("settings-directory-server-dc", dc = dc.clone()),
            Some(None) => tr!("settings-directory-server-no-domain"),
            None => tr!("settings-directory-server-hint"),
        };
        let login = seg_group(
            [
                (DirectoryLogin::Windows, "settings-directory-login-windows"),
                (DirectoryLogin::Password, "settings-directory-login-password"),
                (DirectoryLogin::Anonymous, "settings-directory-login-anonymous"),
            ]
            .map(|(v, key)| {
                seg(("settings-directory-login", v as usize), tr!(key), p.login == v, true, l).on_click(cx.listener(move |this, _, _, cx| {
                    crate::prefs::update(cx, |p| p.security.directory.login = v);
                    this.dir.check = None;
                    cx.notify();
                }))
            }),
            l,
        );
        let check_hint = match &self.dir.check {
            Some(None) => tr!("settings-directory-checking"),
            Some(Some(t)) => t.clone(),
            None => tr!("settings-directory-check-hint"),
        };
        let field =
            |id: &'static str, state: &Entity<InputState>| div().relative().children(crate::screens::spot(id)).w(px(280.)).child(Input::new(state));
        div()
            .flex()
            .flex_col()
            .child(group(tr!("settings-directory-group"), l))
            .child(row(tr!("settings-directory-server"), Some(server_hint), field("directory-server", &self.dir.server), l))
            .child(row(tr!("settings-directory-base"), Some(tr!("settings-directory-base-hint")), field("directory-base", &self.dir.base), l))
            .child(row(
                tr!("settings-directory-login"),
                Some(tr!(match p.login {
                    DirectoryLogin::Windows => "settings-directory-login-windows-hint",
                    DirectoryLogin::Password => "settings-directory-login-password-hint",
                    DirectoryLogin::Anonymous => "settings-directory-login-anonymous-hint",
                })),
                login,
                l,
            ))
            .when(p.login == DirectoryLogin::Password, |d| {
                d.child(row(tr!("settings-directory-user"), None, field("directory-user", &self.dir.user), l)).child(row(
                    tr!("settings-directory-password"),
                    Some(tr!(if self.dir.saved { "settings-directory-password-saved" } else { "settings-directory-password-hint" })),
                    field("directory-password", &self.dir.password),
                    l,
                ))
            })
            .child(row(
                tr!("settings-directory-starttls"),
                Some(tr!("settings-directory-starttls-hint")),
                toggle("settings-directory-starttls", p.starttls, l).on_click(cx.listener(|this, _, _, cx| {
                    crate::prefs::update(cx, |p| p.security.directory.starttls = !p.security.directory.starttls);
                    this.dir.check = None;
                    cx.notify();
                })),
                l,
            ))
            .child(row(
                tr!("settings-directory-check"),
                Some(check_hint),
                button("settings-directory-check", tr!("settings-directory-check-button"), false, l)
                    .on_click(cx.listener(|this, _, _, cx| this.check_directory(cx))),
                l,
            ))
    }
}
