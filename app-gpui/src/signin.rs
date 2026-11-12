//! Добавление аккаунта на месте (S5-09, DESIGN «Настройки»: «Первый запуск — добавление аккаунта на месте»; макет
//! `FirstRun`, T2-28): три шага — адрес почты, сервер (найден по адресу: Exchange, JMAP или IMAP; иначе — вручную) с
//! паролем, проверка (подключение, авторизация, папки) и «Открыть почту». Не модальное окно: при первом запуске — на месте экрана почты
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
    /// Адрес почты (он же логин).
    Address,
    /// Сервер — найден по адресу или вручную; пароль и где его хранить.
    Server,
    /// Проверка: подключение, авторизация, папки.
    Check,
}

impl Step {
    const ALL: [Step; 3] = [Step::Address, Step::Server, Step::Check];

    fn index(self) -> usize {
        match self {
            Step::Address => 0,
            Step::Server => 1,
            Step::Check => 2,
        }
    }

    fn title(self) -> String {
        match self {
            Step::Address => tr!("signin-step-address"),
            Step::Server => tr!("signin-step-server"),
            Step::Check => tr!("signin-step-check"),
        }
    }
}

/// Сервер, найденный по адресу почты (T2-28): адрес пути, как нашли, за сколько.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Found {
    pub(crate) url: String,
    pub(crate) ms: u128,
}

/// Вид сервера по адресу пути: «Exchange (EWS)», «IMAP/SMTP», «JMAP» — и чем нашли.
pub(crate) fn kind_of(url: &str) -> (String, String) {
    if postman_backend_ews::is_ews_url(url) {
        (tr!("signin-kind-ews"), tr!("signin-how-autodiscover"))
    } else if url.starts_with("imaps://") || url.starts_with("imap://") {
        (tr!("signin-kind-imap"), tr!("signin-how-port"))
    } else {
        (tr!("signin-kind-jmap"), tr!("signin-how-wellknown"))
    }
}

/// Узел для строки «Найдены настройки: … · mail.example.ru».
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', ':']).next().unwrap_or(rest).to_owned()
}

pub(crate) struct SignIn {
    pub(crate) step: Step,
    pub(crate) url: Entity<InputState>,
    pub(crate) user: Entity<InputState>,
    password: Entity<InputState>,
    /// Пароль — в диспетчер учётных данных Windows (`false` — не хранить, спрашивать при запуске).
    pub(crate) store: bool,
    /// Машина в домене: у Exchange — выбор «Войти как пользователь Windows» (T1-02б).
    pub(crate) domain: bool,
    /// Выбран вход Windows: шага «Пароль» нет, пароль пустой.
    pub(crate) windows: bool,
    /// Первый запуск: приветствие, тема и плотность; иначе — карточка в настройках с «Отмена».
    welcome: bool,
    /// Вход идёт: ждём сервер.
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    /// Поле шага — в фокус на следующем кадре (нужно окно).
    focus: bool,
    /// Поиск сервера по адресу (T2-28): идёт; нашли; «Настроить вручную» — поле адреса сервера.
    pub(crate) detecting: bool,
    pub(crate) found: Option<Found>,
    pub(crate) manual: bool,
    /// Проверка (шаг 3): вход подтверждён; папка, которая загружается.
    pub(crate) signed: bool,
    pub(crate) folder: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// «Открыть почту» или «Пропустить» (T2-28): панель — прочь.
pub(crate) struct Done;

impl EventEmitter<Done> for SignIn {}

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
        // Адрес и логин уже есть (сервер отклонил сохранённый вход) — сразу шаг сервера с паролем.
        let step = if prefill.user.is_empty() { Step::Address } else { Step::Server };
        let manual = !prefill.url.is_empty();
        let domain = crate::sys::shell::in_domain();
        let windows = prefill.windows;
        Self {
            step,
            url,
            user,
            password,
            store: true,
            domain,
            windows,
            welcome,
            busy: false,
            error: None,
            focus: true,
            detecting: false,
            found: None,
            manual,
            signed: false,
            folder: None,
            _subscriptions: subscriptions,
        }
    }

    /// Первый запуск (панель на месте почты), а не карточка в настройках.
    pub(crate) fn is_welcome(&self) -> bool {
        self.welcome
    }

    /// Найти сервер по домену адреса (T2-28): `домен`, затем `mail.домен` — пути по очереди (JMAP, IMAP, Exchange); в
    /// фоне. Нашли — адрес в поле сервера; нет — поле открыто.
    fn detect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.user.read(cx).value().trim().to_owned();
        let Some((_, domain)) = email.rsplit_once('@') else { return };
        let domain = domain.to_lowercase();
        self.detecting = true;
        self.found = None;
        let task = cx.background_spawn(async move {
            let started = std::time::Instant::now();
            for host in [domain.clone(), format!("mail.{domain}")] {
                let url = crate::plugins::registry().detect_blocking(&host);
                if url != host {
                    return Some(Found { url, ms: started.elapsed().as_millis() });
                }
            }
            None
        });
        cx.spawn_in(window, async move |this, cx| {
            let found = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.detecting = false;
                match found {
                    Some(f) => {
                        this.url.update(cx, |s, cx| s.set_value(f.url.clone(), window, cx));
                        this.found = Some(f);
                    }
                    None => this.manual = true,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Вход подтверждён (шаг «Проверка»): подключение и авторизация — готово, дальше папки.
    pub(crate) fn signed_in(&mut self, cx: &mut Context<Self>) {
        self.busy = false;
        self.signed = true;
        self.step = Step::Check;
        cx.notify();
    }

    /// Загружается папка (шаг «Проверка»).
    pub(crate) fn loading(&mut self, folder: String, cx: &mut Context<Self>) {
        self.folder = Some(folder);
        cx.notify();
    }

    /// Выбор входа Windows виден: адрес — EWS Exchange, машина в домене.
    fn windows_offered(&self, cx: &App) -> bool {
        self.domain && postman_backend_ews::is_ews_url(&normalize_url(&self.url.read(cx).value()))
    }

    /// «Войти как пользователь Windows»: логин — UPN из Windows (в фоне; можно поправить).
    fn set_windows(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.windows = on;
        self.error = None;
        if on && self.user.read(cx).value().trim().is_empty() {
            let task = cx.background_spawn(async { crate::sys::shell::upn() });
            cx.spawn_in(window, async move |this, cx| {
                if let Some(upn) = task.await {
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.user.read(cx).value().trim().is_empty() {
                            this.user.update(cx, |s, cx| s.set_value(upn, window, cx));
                        }
                    });
                }
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.error = None;
        self.focus = true;
        cx.notify();
    }

    /// «Продолжить» или Enter: проверить поле шага и дальше; на последнем — вход.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.step {
            Step::Address => {
                let email = self.user.read(cx).value().trim().to_owned();
                if !email.contains('@') || email.ends_with('@') {
                    return self.fail(tr!("signin-fill-email"), cx);
                }
                if !self.manual {
                    self.detect(window, cx);
                }
                self.go(Step::Server, cx);
            }
            Step::Server => self.submit(cx),
            Step::Check => {
                if self.signed {
                    cx.emit(Done);
                }
            }
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let prev = match self.step {
            Step::Address | Step::Server => Step::Address,
            Step::Check => Step::Server,
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
        let windows = self.windows && self.windows_offered(cx);
        // Вход Windows: пустой пароль — путь EWS входит учётными данными текущего входа (T1-02а).
        let password = if windows { String::new() } else { self.password.read(cx).value().to_string() };
        if user.is_empty() || (password.is_empty() && !windows) {
            return self.fail(tr!("signin-fill"), cx);
        }
        self.busy = true;
        self.error = None;
        // Проверка — шагом 3 (макет): подключение идёт.
        if self.welcome {
            self.step = Step::Check;
        }
        cx.notify();
        cx.emit(Submit { account: Account { url, auth: Auth::Basic { user, password } }, store: self.store });
    }

    /// Сервер отклонил сохранённый вход: причина под полями, адрес и логин уже подставлены.
    pub(crate) fn rejected(&mut self, text: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.step = Step::Server;
        self.error = Some(text);
        cx.notify();
    }

    /// Отказ без попытки входа: текст показывается как есть (учётная запись уже подключена).
    pub(crate) fn refused(&mut self, text: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(text);
        cx.notify();
    }

    /// Вход не удался: показать причину, поля остаются для новой попытки. Вход Windows не удался (T1-02а) — строкой
    /// и предложением войти с паролем: шаг «Пароль».
    pub(crate) fn failed(&mut self, error: &str, cx: &mut Context<Self>) {
        self.busy = false;
        self.step = Step::Server;
        if self.windows && error.contains(postman_backend_ews::WINDOWS_SIGN_IN_UNAVAILABLE) {
            self.windows = false;
            self.focus = true;
            self.error = Some(tr!("signin-windows-failed"));
        } else {
            self.error = Some(tr!("signin-failed", error = error.to_owned()));
        }
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
        let id: &'static str = if to == Step::Address { "signin-edit-address" } else { "signin-edit-server" };
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().w(px(110.)).flex_none().whitespace_nowrap().text_color(l.text2()).child(label))
            .child(div().flex_1().min_w_0().truncate().child(value))
            .child(
                div()
                    .id(id)
                    .children(crate::screens::spot(id))
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
        let row = div().id(id).children(crate::screens::spot(id)).flex().items_center().gap_2().cursor_pointer();
        row.child(crate::settings_tab::radio(on, l)).child(label).on_click(cx.listener(move |this, _, _, cx| {
            this.store = store;
            cx.notify();
        }))
    }

    fn content(&self, l: &crate::roles::Look, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, label: String, primary: bool| crate::buttons::plain(id, label, primary);
        let mut body = div().flex().flex_col().gap_4();
        let user = self.user.read(cx).value().trim().to_owned();
        match self.step {
            Step::Address => {
                body = body.child(self.field(l, tr!("signin-email"), &self.user, Some(tr!("signin-email-hint")))).child(
                    div().flex().child(
                        div()
                            .id("signin-manual")
                            .children(crate::screens::spot("signin-manual"))
                            .px_1()
                            .rounded(l.px(Metric::RadiusControl))
                            .text_size(px(13.))
                            .text_color(l.text2())
                            .cursor_pointer()
                            .hover(|d| d.bg(l.hover()).text_color(l.accent_text()))
                            .child(tr!("signin-manual"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.manual = true;
                                this.next(window, cx);
                            })),
                    ),
                );
            }
            Step::Server => {
                body = body.child(self.summary(l, tr!("signin-address"), user, Step::Address, cx));
                // Найдены настройки (макет): вид и узел, справа — чем и за сколько.
                let line = if self.detecting {
                    Some(div().text_color(l.text2()).child(tr!("signin-detecting")))
                } else {
                    self.found.as_ref().filter(|_| !self.manual).map(|f| {
                        let (kind, how) = kind_of(&f.url);
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .rounded(l.px(Metric::RadiusControl))
                            .border_1()
                            .border_color(l.border())
                            .child(crate::chrome::icon(crate::calendar::g::CHECK, 14.).text_color(l.success()))
                            .child(div().flex_1().min_w_0().truncate().child(tr!("signin-found", kind = kind, host = host_of(&f.url))))
                            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!(
                                "signin-found-how",
                                how = how,
                                secs = format!("{:.1}", f.ms as f64 / 1000.).replace('.', ",")
                            )))
                    })
                };
                body = body.children(line);
                if self.manual || (!self.detecting && self.found.is_none()) {
                    body = body.child(self.field(l, tr!("signin-url"), &self.url, Some(tr!("signin-url-hint"))));
                }
                // Exchange в домене (T1-02б): вход учётной записью Windows без пароля.
                if self.windows_offered(cx) {
                    let on = self.windows;
                    let choice = |id: &'static str, value: bool, label: String| {
                        div()
                            .id(id)
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .child(crate::settings_tab::radio(on == value, l))
                            .child(label)
                            .on_click(cx.listener(move |this, _, window, cx| this.set_windows(value, window, cx)))
                    };
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(choice("signin-windows", true, tr!("signin-windows")))
                            .child(choice("signin-with-password", false, tr!("signin-with-password")))
                            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("signin-windows-hint"))),
                    );
                }
                if !(self.windows && self.windows_offered(cx)) {
                    let (tls, ok) = security_line();
                    body = body
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
            Step::Check => {
                let url = normalize_url(&self.url.read(cx).value());
                let (kind, _) = kind_of(&url);
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(user))
                        .child(div().text_size(px(12.)).text_color(l.text2()).child(format!("{kind} · {}", host_of(&url)))),
                );
                // Ход проверки (макет): готово — ✓, идёт — акцентом, впереди — серым.
                let state = |done: bool, active: bool| -> (char, Hsla) {
                    if done {
                        (crate::calendar::g::CHECK, l.success())
                    } else if active {
                        ('\u{E895}', l.accent_text())
                    } else {
                        ('\u{EA3A}', l.text3())
                    }
                };
                let folders_done = false;
                let rows = [
                    (tr!("signin-check-connect"), self.signed, !self.signed, String::new()),
                    (tr!("signin-check-auth"), self.signed, !self.signed, if self.signed { tr!("signin-check-ok") } else { String::new() }),
                    (tr!("signin-check-folders"), folders_done, self.signed, self.folder.clone().unwrap_or_default()),
                ];
                let list = div().flex().flex_col().rounded(l.px(Metric::RadiusControl)).border_1().border_color(l.border()).children(
                    rows.into_iter().enumerate().map(|(i, (title, done, active, meta))| {
                        let (glyph, color) = state(done, active);
                        div()
                            .id(("signin-check", i))
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .child(crate::chrome::icon(glyph, 14.).text_color(color))
                            .child(div().flex_1().when(active, |d| d.font_weight(FontWeight::SEMIBOLD)).child(title))
                            .child(div().text_size(px(12.)).text_color(if active { l.accent_text() } else { l.text2() }).child(meta))
                    }),
                );
                body = body.child(list).child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("signin-check-hint")));
            }
        }
        let busy = self.busy;
        let primary = match (self.step, busy) {
            (Step::Server, true) | (Step::Check, true) => tr!("signin-busy"),
            (Step::Server, false) => tr!("signin-submit"),
            (Step::Check, false) => tr!("signin-open"),
            _ => tr!("signin-next"),
        };
        let enabled = !busy && (self.step != Step::Check || self.signed);
        let actions = div()
            .flex()
            .items_center()
            .gap_3()
            .child(button("signin-next", primary, enabled).on_click(cx.listener(|this, _, window, cx| this.next(window, cx))))
            .when(self.step != Step::Address && !busy, |d| {
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
        // Клавиши (T2-28, макет): Outlook или одиночные, как в Gmail («Настройки → Клавиатура»).
        let scheme = crate::prefs::get(cx).key_scheme;
        let key_items =
            [(postman_core::prefs::KeyScheme::Outlook, "signin-keys-outlook"), (postman_core::prefs::KeyScheme::Single, "signin-keys-single")]
                .into_iter()
                .enumerate()
                .map(|(i, (s, key))| {
                    seg(("first-keys", i), tr!(key), scheme == s, true, l).on_click(cx.listener(move |_, _, _, cx| {
                        crate::prefs::update(cx, |p| p.key_scheme = s);
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
            .child(group(tr!("signin-keys"), seg_group(key_items, l)))
    }
}

impl Render for SignIn {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        if std::mem::take(&mut self.focus) {
            let input = match self.step {
                Step::Address => &self.user,
                Step::Server if self.manual => &self.url,
                Step::Server | Step::Check => &self.password,
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.))
                    .text_color(l.text3())
                    // «Пропустить» (T2-28, макет): пустое окно; аккаунт — потом, в «Настройки → Аккаунты».
                    .child(
                        div()
                            .id("signin-skip")
                            .children(crate::screens::spot("signin-skip"))
                            .text_color(l.accent_text())
                            .cursor_pointer()
                            .child(tr!("signin-skip"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Done))),
                    )
                    .child("·")
                    .child(tr!(crate::sys::os_key("signin-demo"))),
            )
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
        // Шаг «Логин» у Exchange в домене (T1-02б): «Войти как пользователь Windows (без пароля)».
        Scene::new("signin", signin, signin_ready),
        // Первый запуск по макету `FirstRun` (T2-28): шаг 2 — найдены настройки Exchange; шаг 3 — проверка.
        Scene::new("firstrun-server", step_server, step_ready),
        Scene::new("firstrun-check", step_check, step_ready),
    ];

    fn step(m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>, check: bool) -> Result<(), String> {
        if m.live.signin.is_none() {
            m.show_signin(None, window, cx);
        }
        let panel = m.live.signin.clone().ok_or("no sign-in panel")?;
        panel.update(cx, |p, cx| {
            p.user.update(cx, |s, cx| s.set_value("a.antropov@corp.example", window, cx));
            p.url.update(cx, |s, cx| s.set_value("https://mail.corp.example/EWS/Exchange.asmx", window, cx));
            p.found = Some(super::Found { url: "https://mail.corp.example/EWS/Exchange.asmx".into(), ms: 800 });
            p.manual = false;
            p.detecting = false;
            p.domain = false;
            p.error = None;
            p.signed = check;
            p.folder = check.then(|| "Входящие · 1 240 / 3 900".to_owned()); // i18n-ok: демо-ход загрузки
            p.go(if check { super::Step::Check } else { super::Step::Server }, cx);
        });
        Ok(())
    }

    fn step_server(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        step(m, window, cx, false)
    }

    fn step_check(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        step(m, window, cx, true)
    }

    fn step_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        let want = if s.name == "firstrun-check" { super::Step::Check } else { super::Step::Server };
        m.live.signin.as_ref().is_some_and(|p| p.read(cx).step == want)
    }

    fn signin(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let prefill = crate::account::Settings {
            url: "https://mail.corp.example/EWS/Exchange.asmx".into(),
            user: "a.antropov@corp.example".into(),
            windows: true,
        };
        m.show_signin(Some(prefill), window, cx);
        if let Some(panel) = &m.live.signin {
            panel.update(cx, |p, cx| {
                // Снимок не зависит от машины: «в домене».
                p.domain = true;
                p.go(super::Step::Server, cx);
            });
        }
        Ok(())
    }

    fn signin_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.live.signin.as_ref().is_some_and(|p| p.read(cx).windows && p.read(cx).domain)
    }

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
