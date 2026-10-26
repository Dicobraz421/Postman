//! Добавление аккаунта на месте (S5-09, DESIGN «Настройки»: «Первый запуск — добавление аккаунта на месте»): три шага —
//! адрес сервера, логин, пароль — и проверка входом. Не модальное окно: при первом запуске — на месте экрана почты
//! (с выбором темы и плотности), в «Настройки → Аккаунты» — карточкой в списке аккаунтов. Вход проверяется
//! синхронизацией: панель уходит после первого ответа сервера.
//!
//! Пароль — только в диспетчере учётных данных Windows или нигде («Не хранить — спрашивать при запуске»); Postman
//! сам секреты на диск не пишет.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Themes};
use postman_core::model::Auth;
use postman_core::sync::Account;

use crate::account::Settings;
use crate::settings_tab::{seg, seg_group};

/// Шаг добавления.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Server,
    Login,
    Password,
}

impl Step {
    const ALL: [Step; 3] = [Step::Server, Step::Login, Step::Password];

    fn index(self) -> usize {
        match self {
            Step::Server => 0,
            Step::Login => 1,
            Step::Password => 2,
        }
    }

    fn title(self) -> String {
        match self {
            Step::Server => tr!("signin-step-server"),
            Step::Login => tr!("signin-step-login"),
            Step::Password => tr!("signin-step-password"),
        }
    }
}

pub(crate) struct SignIn {
    step: Step,
    url: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    /// Пароль — в диспетчер учётных данных Windows (`false` — не хранить, спрашивать при запуске).
    store: bool,
    /// Первый запуск: приветствие, тема и плотность; иначе — карточка в настройках с «Отмена».
    welcome: bool,
    /// Вход идёт: ждём сервер.
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    /// Поле шага — в фокус на следующем кадре (нужно окно).
    focus: bool,
    _subscriptions: Vec<Subscription>,
}

/// «Проверить и подключить»: учётная запись из полей и где хранить пароль.
pub(crate) struct Submit {
    pub(crate) account: Account,
    pub(crate) store: bool,
}

/// «Отмена» в настройках.
pub(crate) struct Cancel;

impl EventEmitter<Submit> for SignIn {}
impl EventEmitter<Cancel> for SignIn {}

impl SignIn {
    pub(crate) fn new(prefill: Option<Settings>, welcome: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prefill = prefill.unwrap_or_default();
        let url = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("signin-url-placeholder")).default_value(prefill.url.clone()));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("signin-user-placeholder")).default_value(prefill.user.clone()));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let enter = |this: &mut Self, _: &Entity<InputState>, ev: &InputEvent, window: &mut Window, cx: &mut Context<Self>| {
            if let InputEvent::PressEnter { .. } = ev {
                this.next(window, cx);
            }
        };
        let subscriptions =
            vec![cx.subscribe_in(&url, window, enter), cx.subscribe_in(&user, window, enter), cx.subscribe_in(&password, window, enter)];
        // Адрес и логин уже есть (сервер отклонил сохранённый вход) — сразу пароль.
        let step = match (prefill.url.is_empty(), prefill.user.is_empty()) {
            (true, _) => Step::Server,
            (false, true) => Step::Login,
            (false, false) => Step::Password,
        };
        Self { step, url, user, password, store: true, welcome, busy: false, error: None, focus: true, _subscriptions: subscriptions }
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.error = None;
        self.focus = true;
        cx.notify();
    }

    /// «Продолжить» или Enter: проверить поле шага и дальше; на последнем — вход.
    fn next(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.step {
            Step::Server => match check_url(&self.url.read(cx).value()) {
                Ok(_) => self.go(Step::Login, cx),
                Err(e) => self.fail(e, cx),
            },
            Step::Login if self.user.read(cx).value().trim().is_empty() => self.fail(tr!("signin-fill-user"), cx),
            Step::Login => self.go(Step::Password, cx),
            Step::Password => self.submit(cx),
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let prev = match self.step {
            Step::Server | Step::Login => Step::Server,
            Step::Password => Step::Login,
        };
        self.go(prev, cx);
    }

    fn fail(&mut self, text: String, cx: &mut Context<Self>) {
        self.error = Some(text);
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let url = match check_url(&self.url.read(cx).value()) {
            Ok(u) => u,
            Err(e) => return self.fail(e, cx),
        };
        let user = self.user.read(cx).value().trim().to_owned();
        let password = self.password.read(cx).value().to_string();
        if user.is_empty() || password.is_empty() {
            return self.fail(tr!("signin-fill"), cx);
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.emit(Submit { account: Account { url, auth: Auth::Basic { user, password } }, store: self.store });
    }

    /// Сервер отклонил сохранённый вход: причина под полями, адрес и логин уже подставлены.
    pub(crate) fn rejected(&mut self, text: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.step = Step::Password;
        self.error = Some(text);
        cx.notify();
    }

    /// Вход не удался: показать причину, поля остаются для новой попытки.
    pub(crate) fn failed(&mut self, error: &str, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(tr!("signin-failed", error = error.to_owned()));
        cx.notify();
    }
}

/// Адрес сервера: без пробелов и `/` в конце; без схемы — `https://`. `imaps://` и `imap://` — учётная запись IMAP
/// (S7-04б).
pub(crate) fn normalize_url(raw: &str) -> String {
    let url = raw.trim().trim_end_matches('/');
    if url.is_empty() || url.starts_with("http://") || url.starts_with("https://") || crate::plugins::registry().claims(url) {
        url.to_owned()
    } else {
        format!("https://{url}")
    }
}

/// Адрес сервера годится: не пустой, разбирается как URL с именем узла.
fn check_url(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(tr!("signin-fill-url"));
    }
    let rest = ["https://", "http://", "imaps://", "imap://"].iter().find_map(|p| t.strip_prefix(p)).unwrap_or(t);
    if rest.trim_matches('/').is_empty() {
        return Err(tr!("signin-bad-url"));
    }
    let url = normalize_url(raw);
    let host = url.split_once("://").map(|(_, rest)| rest.split(['/', ':', '?']).next().unwrap_or_default());
    if host.is_none_or(|h| h.is_empty() || h.contains(' ')) {
        return Err(tr!("signin-bad-url"));
    }
    Ok(url)
}

/// Строка «Безопасность подключения»: TLS и проверка сертификата (`POSTMAN_INSECURE_TLS` — стенд — предупреждение).
pub(crate) fn security_line() -> (String, bool) {
    if postman_core::backend::insecure_tls() { (tr!("signin-tls-insecure"), false) } else { (tr!("signin-tls"), true) }
}

impl SignIn {
    fn stepper(&self, l: &crate::roles::Look) -> Div {
        let current = self.step.index();
        let mut row = div().flex().items_center().gap_2();
        for (i, s) in Step::ALL.iter().enumerate() {
            let (done, active) = (i < current, i == current);
            let dot = div()
                .size(px(22.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(px(12.))
                .when(done || active, |d| d.bg(l.accent()).text_color(l.on_accent()))
                .when(!done && !active, |d| d.border_1().border_color(l.stroke()).text_color(l.text2()))
                .child(if done { "✓".to_string() } else { (i + 1).to_string() });
            let label = div().when(active, |d| d.font_weight(FontWeight::SEMIBOLD)).when(!active, |d| d.text_color(l.text2())).child(s.title());
            row = row.child(div().flex().items_center().gap_2().child(dot).child(label));
            if i + 1 < Step::ALL.len() {
                row = row.child(div().w(px(40.)).h(px(1.)).bg(if done { l.accent() } else { l.border() }));
            }
        }
        row
    }

    /// Уже введённое на прошлых шагах: строка «Сервер: … · Изменить».
    fn summary(&self, l: &crate::roles::Look, label: String, value: String, to: Step, cx: &mut Context<Self>) -> Div {
        let id: &'static str = if to == Step::Server { "signin-edit-server" } else { "signin-edit-login" };
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().w(px(90.)).flex_none().text_color(l.text2()).child(label))
            .child(div().flex_1().min_w_0().truncate().child(value))
            .child(
                div()
                    .id(id)
                    .px_2()
                    .rounded(l.px(Metric::RadiusControl))
                    .text_color(l.accent_text())
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(tr!("signin-change"))
                    .on_click(cx.listener(move |this, _, _, cx| this.go(to, cx))),
            )
    }

    fn field(&self, l: &crate::roles::Look, label: String, input: &Entity<InputState>, hint: Option<String>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_color(l.text2()).child(label))
            .child(Input::new(input))
            .when_some(hint, |d, h| d.child(div().text_size(px(12.)).text_color(l.text2()).child(h)))
    }

    fn radio_row(&self, l: &crate::roles::Look, id: &'static str, on: bool, label: String, cx: &mut Context<Self>) -> Stateful<Div> {
        let store = id == "signin-store-windows";
        div().id(id).flex().items_center().gap_2().cursor_pointer().child(crate::settings_tab::radio(on, l)).child(label).on_click(cx.listener(
            move |this, _, _, cx| {
                this.store = store;
                cx.notify();
            },
        ))
    }

    fn content(&self, l: &crate::roles::Look, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, label: String, primary: bool| crate::buttons::plain(id, label, primary);
        let mut body = div().flex().flex_col().gap_4();
        let url = normalize_url(&self.url.read(cx).value());
        let user = self.user.read(cx).value().trim().to_owned();
        match self.step {
            Step::Server => {
                body = body.child(self.field(l, tr!("signin-url"), &self.url, Some(tr!("signin-url-hint"))));
            }
            Step::Login => {
                body = body.child(self.summary(l, tr!("signin-url"), url, Step::Server, cx)).child(self.field(
                    l,
                    tr!("signin-user"),
                    &self.user,
                    Some(tr!("signin-user-hint")),
                ));
            }
            Step::Password => {
                let (tls, ok) = security_line();
                body = body
                    .child(self.summary(l, tr!("signin-url"), url, Step::Server, cx))
                    .child(self.summary(l, tr!("signin-user"), user, Step::Login, cx))
                    .child(self.field(l, tr!("signin-password"), &self.password, None))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().text_color(l.text2()).child(tr!("signin-store")))
                            .child(self.radio_row(l, "signin-store-windows", self.store, tr!("signin-store-windows"), cx))
                            .child(self.radio_row(l, "signin-store-never", !self.store, tr!("signin-store-never"), cx))
                            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("signin-store-note"))),
                    )
                    .child(
                        div()
                            .p_2()
                            .rounded(l.px(Metric::RadiusControl))
                            .bg(if ok { l.fill_passive() } else { l.warning_bg() })
                            .text_size(px(12.))
                            .text_color(if ok { l.text2() } else { l.warning() })
                            .child(tls),
                    );
            }
        }
        let busy = self.busy;
        let primary = match (self.step, busy) {
            (Step::Password, true) => tr!("signin-busy"),
            (Step::Password, false) => tr!("signin-submit"),
            _ => tr!("signin-next"),
        };
        let actions = div()
            .flex()
            .items_center()
            .gap_3()
            .child(button("signin-next", primary, !busy).on_click(cx.listener(|this, _, window, cx| this.next(window, cx))))
            .when(self.step != Step::Server, |d| {
                d.child(button("signin-back", tr!("signin-back"), false).on_click(cx.listener(|this, _, _, cx| this.back(cx))))
            })
            .when(!self.welcome, |d| {
                d.child(button("signin-cancel", tr!("signin-cancel"), false).on_click(cx.listener(|_, _, _, cx| cx.emit(Cancel))))
            });
        body.when_some(self.error.clone(), |d, e| d.child(div().text_color(l.error()).child(e))).child(actions)
    }

    /// Тема и плотность при первом запуске — те же, что в «Настройки → Общие».
    fn first_prefs(&self, l: &crate::roles::Look, cx: &mut Context<Self>) -> Div {
        let (choice, locked) =
            Themes::try_get(cx).map_or((pets_ui::theme::SYSTEM.to_owned(), false), |t| (t.state().choice().to_owned(), t.state().locked()));
        let row = crate::roles::look(cx).density().row();
        let themes = [pets_ui::theme::SYSTEM, pets_ui::theme::FLUENT_LIGHT, pets_ui::theme::FLUENT_DARK];
        let theme_items = themes
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let title = Themes::try_get(cx).map_or_else(|| id.into(), |t| pets_ui::theme::title(id, t.state()));
                seg(("first-theme", i), title.to_string(), choice == id, !locked, l).on_click(cx.listener(move |_, _, _, cx| {
                    if let Err(e) = crate::prefs::set_theme(id, cx) {
                        tracing::info!("theme: {e}");
                    }
                    cx.notify();
                }))
            })
            .collect::<Vec<_>>();
        let densities = [(24u16, "settings-density-normal"), (20, "settings-density-compact")];
        let density_items = densities
            .iter()
            .map(|&(px_row, key)| {
                seg(("first-density", px_row as usize), tr!(key, px = px_row), row == px_row, true, l).on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::set_density(px_row, cx);
                    cx.notify();
                }))
            })
            .collect::<Vec<_>>();
        let group = |label: String, items: Div| div().flex().items_center().gap_3().child(div().text_color(l.text2()).child(label)).child(items);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_center()
            .gap_6()
            .child(group(tr!("settings-theme"), seg_group(theme_items, l)))
            .child(group(tr!("settings-density"), seg_group(density_items, l)))
    }
}

impl Render for SignIn {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        if std::mem::take(&mut self.focus) {
            let input = match self.step {
                Step::Server => &self.url,
                Step::Login => &self.user,
                Step::Password => &self.password,
            };
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        let card = div()
            .when(self.welcome, |d| d.w(px(560.)))
            .when(!self.welcome, |d| d.w_full())
            .flex()
            .flex_col()
            .gap_5()
            .p_6()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.panel())
            .when(self.welcome, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).child(tr!("signin-title")))
                        .child(div().text_color(l.text2()).child(tr!("signin-hint"))),
                )
            })
            .when(!self.welcome, |d| {
                d.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(tr!("signin-new")))
                        .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("signin-new-hint"))),
                )
            })
            .child(self.stepper(&l))
            .child(self.content(&l, cx));
        if !self.welcome {
            return div().w_full().child(card).into_any_element();
        }
        div()
            .id("first-run")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .overflow_y_scroll()
            .p_6()
            .child(card)
            .child(self.first_prefs(&l, cx))
            .child(div().text_size(px(12.)).text_color(l.text3()).child(tr!("signin-demo")))
            .into_any_element()
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "signin", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/account.ftl")), ("en", include_str!("../i18n/en/account.ftl"))];

/// Сцены `--screens` первого запуска (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::Scene;
    use crate::{Mailer, Section};

    /// Имя сцены строки входа: её одну снимает `--screens --live` без учётной записи в окружении.
    pub(crate) const FIRSTRUN: &str = "firstrun";

    pub(crate) const SCENES: &[Scene] = &[
        // Первый запуск (S5-09): добавление аккаунта на месте экрана почты (сроки прошлых сцен сняты).
        Scene::new(FIRSTRUN, firstrun, firstrun_ready),
    ];

    fn firstrun(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Уведомления сцены `toasts` — убрать: первый запуск без них.
        m.toasts.update(cx, |t, cx| {
            t.hide_all(cx);
            if t.is_history_open() {
                t.toggle_history(cx);
            }
        });
        // Сроки сцены `lifecycle` — снять: дальше снимки без них.
        let undo = m.screens.as_mut().map(|d| std::mem::take(&mut d.undo)).unwrap_or_default();
        m.apply_patches(undo, cx);
        m.close_settings(window, cx);
        m.set_section(Section::Mail, cx);
        // Демо: первый запуск без учётной записи — так же, как без `POSTMAN_URL` и сохранённого входа.
        if m.live.signin.is_none() {
            m.show_signin(None, window, cx);
        }
        Ok(())
    }

    fn firstrun_ready(_: &Scene, m: &Mailer, _: &App) -> bool {
        m.live.signin.is_some() && !m.tabs.is_active(crate::tabs::TabKind::Settings)
    }
}

#[cfg(test)]
mod tests {
    use super::{check_url, normalize_url};

    #[test]
    fn server_url_is_normalized() {
        assert_eq!(normalize_url("  http://localhost:18080/ "), "http://localhost:18080");
        assert_eq!(normalize_url("mail.example.com"), "https://mail.example.com");
        assert_eq!(normalize_url("https://mail.example.com//"), "https://mail.example.com");
        assert_eq!(normalize_url("   "), "");
    }

    #[test]
    fn server_url_needs_a_host() {
        assert_eq!(check_url("mail.example.com").as_deref(), Ok("https://mail.example.com"));
        assert!(check_url("").is_err());
        assert!(check_url("https://").is_err());
        assert!(check_url("mail example").is_err());
        // Учётная запись IMAP (S7-04б): схема сохраняется, SMTP — в запросе.
        assert_eq!(check_url("imaps://imap.example.com?smtp=smtp.example.com").as_deref(), Ok("imaps://imap.example.com?smtp=smtp.example.com"));
        assert!(check_url("imaps://").is_err());
    }
}
