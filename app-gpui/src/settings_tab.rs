//! Вкладка «Настройки» (S5-09, `docs/DESIGN.md` «Настройки»): слева — разделы с поиском, справа — раздел. Разделы:
//! Общие (`settings_general.rs`), Аккаунты (`settings_accounts.rs`), Категории (`settings_cats.rs`), Быстрые фильтры
//! и сроки, Чтение, Календарь (`settings_cal.rs`, S5-20), Подписи и шаблоны (`settings_sign.rs`), Клавиатура (`settings_keys.rs`), Правила и Автоответы
//! (панели `rules.rs` и `vacation.rs` — их держит экран почты, `settings.rs`), Безопасность (`settings_security.rs`) и
//! Внешний вид (`settings_look.rs`) — S5-30, порядок разделов — как в макетах «Настройки».
//!
//! Своё — в `postman_core::prefs` через `crate::prefs` (файл пишется в фоне); категории, удаление аккаунта и панели
//! сервера — событиями [`SettingsEvent`] экрану почты.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_core::labels::Categories;
use postman_core::prefs::{Conversation, DAYS, HOURS, Pane, Quick};

use crate::chrome::{glyph, icon};
use crate::roles::Look;

/// Раздел настроек.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    General,
    Accounts,
    Categories,
    Quick,
    Reading,
    Calendar,
    Security,
    Signatures,
    Keys,
    Look,
    Rules,
    Vacation,
}

impl Section {
    /// Порядок макетов «Настройки» (S5-30); подписи, правила и автоответы — после.
    /// Команда раздела, если ему нужно что-то от сервера («Нет у сервера — нет на экране», S7-03).
    fn command(self) -> Option<&'static str> {
        match self {
            Section::Rules => Some("settings.rules"),
            Section::Vacation => Some("settings.vacation"),
            Section::Calendar => Some("settings.calendar"),
            _ => None,
        }
    }

    pub(crate) const ALL: [Section; 12] = [
        Section::General,
        Section::Accounts,
        Section::Reading,
        Section::Calendar,
        Section::Categories,
        Section::Quick,
        Section::Security,
        Section::Keys,
        Section::Look,
        Section::Signatures,
        Section::Rules,
        Section::Vacation,
    ];

    fn key(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Accounts => "accounts",
            Section::Categories => "categories",
            Section::Quick => "quick",
            Section::Reading => "reading",
            Section::Calendar => "calendar",
            Section::Security => "security",
            Section::Look => "look",
            Section::Signatures => "signatures",
            Section::Keys => "keys",
            Section::Rules => "rules",
            Section::Vacation => "vacation",
        }
    }

    pub(crate) fn title(self) -> String {
        tr!(&format!("settings-nav-{}", self.key()))
    }

    fn keywords(self) -> String {
        tr!(&format!("settings-nav-{}-keywords", self.key()))
    }

    fn hint(self) -> String {
        tr!(&format!("settings-hint-{}", self.key()))
    }

    fn glyph(self) -> char {
        match self {
            Section::General => '\u{E771}',
            Section::Accounts => '\u{E77B}',
            Section::Categories => '\u{E8EC}',
            Section::Quick => '\u{E8FD}',
            Section::Reading => '\u{E8A1}',
            Section::Calendar => crate::calendar::g::CALENDAR,
            Section::Security => '\u{E72E}',
            Section::Look => '\u{E790}',
            Section::Signatures => '\u{E8AC}',
            Section::Keys => '\u{E765}',
            Section::Rules => glyph::RULES,
            Section::Vacation => glyph::VACATION,
        }
    }
}

/// Поиск раздела: каждое слово запроса — в названии или ключевых словах раздела (без учёта регистра и «ё»).
pub(crate) fn matches(query: &str, title: &str, keywords: &str) -> bool {
    let fold = |s: &str| s.to_lowercase().replace('ё', "е"); // i18n-ok: поиск без учёта «ё»
    let hay = fold(&format!("{title} {keywords}"));
    query.split_whitespace().all(|w| hay.contains(&fold(w)))
}

/// Разделы по запросу, в порядке навигации; разделов, которых нет у сервера (S7-03), нет и здесь.
pub(crate) fn found(query: &str, features: postman_core::model::Features) -> Vec<Section> {
    Section::ALL
        .into_iter()
        .filter(|s| s.command().is_none_or(|id| crate::features::available(id, features)))
        .filter(|s| matches(query, &s.title(), &s.keywords()))
        .collect()
}

/// Что вкладке нужно знать об экране почты: учётная запись, папки, адреса отправителя.
#[derive(Clone, Default)]
pub(crate) struct Info {
    /// Живой ящик (иначе демо).
    pub(crate) live: bool,
    pub(crate) account: Option<crate::account::Settings>,
    /// «Синхронизировано», «Подключение…».
    pub(crate) status: String,
    /// Общие ящики основной учётной записи (имена).
    pub(crate) shared: Vec<String>,
    /// Дополнительные учётные записи профиля (S7-03б): префикс, адрес и логин, их общие ящики.
    pub(crate) extra: Vec<(String, crate::account::Settings, Vec<String>)>,
    /// Свои папки: id и имя.
    pub(crate) folders: Vec<(String, String)>,
    /// Адреса отправителя (Identity).
    pub(crate) identities: Vec<String>,
    /// Файл кэша учётной записи: рядом — её подписи и шаблоны.
    pub(crate) cache: Option<std::path::PathBuf>,
    /// Песочница последнего разбора письма («Безопасность», S5-30); `None` — ещё не разбирали.
    pub(crate) sandbox: Option<postman_worker::Level>,
}

/// Экрану почты.
pub(crate) enum SettingsEvent {
    /// Открыт раздел: правилам и автоответам нужна панель экрана.
    Opened(Section),
    /// Таблица категорий изменена (своя учётная запись): в список, в локальную копию и на сервер.
    Categories(Categories),
    /// Снять ключевое слово удалённой категории со всех писем аккаунта.
    Purge(String),
    /// «Добавить аккаунт»: экрану — карточку добавления (вход идёт через него).
    AddAccount,
    /// «Удалить аккаунт» подтверждено.
    RemoveAccount,
    /// «Удалить» у дополнительной учётной записи (`@<n>`) подтверждено.
    RemoveExtra(String),
    /// Подписи и шаблоны учётной записи изменены: письму — новые подписи по умолчанию.
    AccountPrefs(postman_core::prefs::AccountPrefs),
}

pub(crate) struct SettingsView {
    pub(crate) section: Section,
    search: Entity<InputState>,
    query: String,
    pub(crate) info: Info,
    pub(crate) cats: Categories,
    pub(crate) cat_edit: Option<crate::settings_cats::CatEdit>,
    /// Ключевое слово удалённой категории, снятие которого с писем ждёт подтверждения.
    pub(crate) cat_purge: Option<String>,
    pub(crate) keys: crate::settings_keys::KeysState,
    pub(crate) sign: crate::settings_sign::SignState,
    /// Карточка добавления аккаунта (её события слушает экран почты).
    pub(crate) add: Option<AnyView>,
    pub(crate) remove_confirm: bool,
    /// Подтверждение удаления дополнительной учётной записи (`@<n>`).
    pub(crate) remove_extra: Option<String>,
    /// Пароль в диспетчере учётных данных (`None` — ещё не проверено).
    pub(crate) stored: Option<bool>,
    pub(crate) rules: Option<AnyView>,
    pub(crate) vacation: Option<AnyView>,
    pub(crate) theme_error: Option<String>,
    /// «Общие → Безопасность» (S5-08): адрес для писем «Сообщить о фишинге»; ошибка — не адрес.
    pub(crate) phishing: Entity<InputState>,
    pub(crate) phishing_error: bool,
    /// «Календарь»: открытый список выбора (часы, пояс, напоминание).
    pub(crate) cal_menu: Option<crate::settings_cal::Menu>,
    /// Прокрутка раздела: новый раздел — сверху; снимок `settings-calendar-free` — внизу.
    pub(crate) scroll: ScrollHandle,
    /// «Общие» (S5-30): автозапуск, клиент `mailto:`, отчёт — из Windows.
    pub(crate) os: crate::settings_general::OsState,
    /// «Безопасность»: строка «Сбросить входы?», входы сброшены, раскрыта подсказка песочницы.
    pub(crate) wipe_confirm: bool,
    pub(crate) wiped: bool,
    pub(crate) sandbox_help: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub(crate) fn new(section: Section, info: Info, cats: Categories, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-search")));
        let subscriptions = vec![cx.subscribe_in(&search, window, |this: &mut Self, state, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                this.query = state.read(cx).value().to_string();
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(&s) = found(&this.query, crate::features::get(cx)).first() {
                    this.select(s, window, cx);
                }
            }
            _ => {}
        })];
        let keys = crate::settings_keys::KeysState::new(window, cx);
        let sign = crate::settings_sign::SignState::new(info.cache.clone(), window, cx);
        let current = crate::prefs::get(cx).general.phishing_report.unwrap_or_default();
        let phishing = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(tr!("settings-phishing-placeholder"));
            s.set_value(current, window, cx);
            s
        });
        let mut subscriptions = subscriptions;
        // Адрес — в настройки по мере ввода: пусто — без пересылки; не адрес — подсказка, настройка — пустая.
        subscriptions.push(cx.subscribe_in(&phishing, window, |this: &mut Self, state, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                let text = state.read(cx).value().to_string();
                let address = postman_core::prefs::phishing_address(Some(&text));
                this.phishing_error = !text.trim().is_empty() && address.is_none();
                crate::prefs::update(cx, |p| p.general.phishing_report = address);
                cx.notify();
            }
        }));
        let mut v = Self {
            section,
            search,
            query: String::new(),
            info,
            cats,
            cat_edit: None,
            cat_purge: None,
            keys,
            sign,
            add: None,
            remove_confirm: false,
            remove_extra: None,
            stored: None,
            rules: None,
            vacation: None,
            theme_error: None,
            phishing,
            phishing_error: false,
            cal_menu: None,
            scroll: ScrollHandle::new(),
            os: Default::default(),
            wipe_confirm: false,
            wiped: false,
            sandbox_help: false,
            _subscriptions: subscriptions,
        };
        v.read_os(cx);
        v.sign_load(cx);
        v.check_password(cx);
        v
    }

    /// Открыть раздел (навигация, `settings.rules`, `settings.vacation`).
    pub(crate) fn select(&mut self, section: Section, _window: &mut Window, cx: &mut Context<Self>) {
        if self.section != section {
            self.stop_recording(cx);
            self.cat_edit = None;
            self.remove_confirm = false;
            self.cal_menu = None;
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.section = section;
        cx.emit(SettingsEvent::Opened(section));
        cx.notify();
    }

    /// Таблица категорий изменилась на сервере или в списке: показать новую (правка на месте остаётся).
    pub(crate) fn set_categories(&mut self, cats: Categories, cx: &mut Context<Self>) {
        self.cats = cats;
        cx.notify();
    }

    pub(crate) fn set_info(&mut self, info: Info, cx: &mut Context<Self>) {
        let cache_changed = info.cache != self.info.cache;
        self.info = info;
        if cache_changed {
            self.sign.path = self.info.cache.clone();
            self.sign_load(cx);
            self.check_password(cx);
        }
        cx.notify();
    }

    /// Esc: остановить запись клавиши или закрыть правку на месте; `false` — нечего (вкладка закрывается).
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.keys.recording.is_some() {
            self.stop_recording(cx);
            return true;
        }
        if self.cat_edit.take().is_some()
            || self.cat_purge.take().is_some()
            || std::mem::take(&mut self.remove_confirm)
            || std::mem::take(&mut self.wipe_confirm)
            || self.cal_menu.take().is_some()
        {
            cx.notify();
            return true;
        }
        false
    }

    fn check_password(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.info.account.clone().filter(|_| self.info.live) else { return };
        let task = cx.background_spawn(async move { crate::account::password_stored(&account) });
        cx.spawn(async move |this, cx| {
            let stored = task.await;
            let _ = this.update(cx, |v, cx| {
                v.stored = Some(stored);
                cx.notify();
            });
        })
        .detach();
    }
}

// --- элементы управления (Fluent) ---

/// Переключатель «вкл/выкл».
pub(crate) fn switch(on: bool, l: &Look) -> Div {
    div()
        .w(px(40.))
        .h(px(20.))
        .flex_none()
        .relative()
        .rounded_full()
        .when(on, |d| d.bg(l.accent()))
        .when(!on, |d| d.border_1().border_color(l.c(Role::StrokeStrong)))
        .child(div().absolute().top(px(if on { 4. } else { 3. })).left(px(if on { 24. } else { 3. })).size(px(12.)).rounded_full().bg(if on {
            l.on_accent()
        } else {
            l.text2()
        }))
}

/// Переключатель с id для нажатия.
pub(crate) fn toggle(id: impl Into<ElementId>, on: bool, l: &Look) -> Stateful<Div> {
    div().id(id).flex_none().cursor_pointer().child(switch(on, l))
}

/// Кружок выбора одного из.
pub(crate) fn radio(on: bool, l: &Look) -> Div {
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .when(on, |d| d.bg(l.accent()).child(div().size(px(6.)).rounded_full().bg(l.on_accent())))
        .when(!on, |d| d.border_1().border_color(l.c(Role::StrokeStrong)))
}

/// Кнопка группы «одно из» (сегмент).
pub(crate) fn seg(id: impl Into<ElementId>, label: String, on: bool, enabled: bool, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(l.px(Metric::RadiusControl))
        .when(on, |d| d.bg(l.panel()).border_1().border_color(l.stroke()).font_weight(FontWeight::SEMIBOLD))
        .when(!on && enabled, |d| d.text_color(l.text2()).cursor_pointer().hover(|d| d.bg(l.hover())))
        .when(!enabled, |d| d.text_color(l.c(Role::TextDisabled)))
        .child(label)
}

/// Группа сегментов на нейтральной подложке.
pub(crate) fn seg_group(items: impl IntoIterator<Item = Stateful<Div>>, l: &Look) -> Div {
    div().flex().flex_wrap().items_center().gap_0p5().p_0p5().rounded(l.px(Metric::RadiusControl)).bg(l.fill_passive()).children(items)
}

/// Строка настройки: название и пояснение слева, управление справа.
pub(crate) fn row(title: String, hint: Option<String>, control: impl IntoElement, l: &Look) -> Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .py_3()
        .border_b_1()
        .border_color(l.border())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().child(title))
                .when_some(hint, |d, h| d.child(div().text_size(px(12.)).text_color(l.text2()).child(h))),
        )
        .child(control)
}

/// Заголовок группы в разделе.
pub(crate) fn group(title: String, l: &Look) -> Div {
    div().pt_5().pb_1().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(l.text()).child(title)
}

/// Клавиша: `Ctrl+R` в рамке.
pub(crate) fn keycap(text: String, l: &Look) -> Div {
    div()
        .flex_none()
        .px_1p5()
        .rounded(l.px(Metric::RadiusControl))
        .border_1()
        .border_color(l.stroke())
        .text_size(px(12.))
        .text_color(l.text())
        .child(text)
}

/// Тихая кнопка-ссылка.
pub(crate) fn link(id: impl Into<ElementId>, label: String, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded(l.px(Metric::RadiusControl))
        .text_color(l.accent_text())
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
        .child(label)
}

/// Карточка сбоку раздела.
pub(crate) fn aside_card(title: String, lines: Vec<String>, l: &Look) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded(l.px(Metric::RadiusOverlay))
        .border_1()
        .border_color(l.c(Role::StrokeCard))
        .bg(l.panel())
        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
        .children(lines.into_iter().map(|t| div().text_color(l.text2()).line_height(px(19.)).child(t)))
}

/// Время правки (Unix, мс) для таблицы категорий и id подписей.
pub(crate) fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

// --- отрисовка ---

impl SettingsView {
    fn nav(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let shown = found(&self.query, crate::features::get(cx));
        let mut list = div().flex().flex_col().gap_0p5();
        for (i, s) in shown.iter().copied().enumerate() {
            let on = s == self.section;
            list = list.child(
                div()
                    .id(("settings-nav", i))
                    .h(px(34.))
                    .px_2p5()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .rounded(l.px(Metric::RadiusControl))
                    .cursor_pointer()
                    .when(on, |d| d.bg(l.fill()).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                    .child(div().w(px(3.)).h(px(16.)).rounded_sm().bg(if on { l.accent() } else { l.none() }))
                    .child(icon(s.glyph(), 16.).text_color(if on { l.accent_text() } else { l.text2() }))
                    .child(s.title())
                    .on_click(cx.listener(move |this, _, window, cx| this.select(s, window, cx))),
            );
        }
        if shown.is_empty() {
            list = list.child(div().px_2p5().py_2().text_color(l.text2()).child(tr!("settings-search-nothing")));
        }
        div()
            .w(px(248.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .px_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(glyph::SEARCH, 14.).text_color(l.text2()))
                    .child(div().flex_1().child(Input::new(&self.search))),
            )
            .child(list)
    }

    fn header(&self, l: &Look, action: Option<Stateful<Div>>) -> Div {
        div()
            .flex()
            .items_end()
            .gap_4()
            .px_6()
            .pt_5()
            .pb_3()
            .border_b_1()
            .border_color(l.border())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(self.section.title()))
                    .child(div().text_color(l.text2()).child(self.section.hint())),
            )
            .children(action)
    }

    fn quick(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = crate::prefs::get(cx);
        let n = p.quick.items.len();
        let mut list = div().flex().flex_col();
        for (i, item) in p.quick.items.iter().copied().enumerate() {
            let q = item.filter;
            let arrow = |id: &'static str, g: char, delta: isize, enabled: bool| {
                div()
                    .id((id, i))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .when(enabled, |d| d.cursor_pointer().hover(|d| d.bg(l.hover())).text_color(l.text2()))
                    .when(!enabled, |d| d.text_color(l.c(Role::TextDisabled)))
                    .child(icon(g, 12.))
                    .when(enabled, |d| {
                        d.on_click(cx.listener(move |_, _, _, cx| {
                            crate::prefs::update(cx, |p| {
                                p.quick.shift(q, delta);
                            });
                            cx.notify();
                        }))
                    })
            };
            let up = arrow("settings-quick-up", '\u{E70E}', -1, i > 0);
            let down = arrow("settings-quick-down", '\u{E70D}', 1, i + 1 < n);
            let control =
                div().flex().items_center().gap_3().child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-quick-shown"))).child(
                    toggle(("settings-quick-toggle", i), item.shown, l).on_click(cx.listener(move |_, _, _, cx| {
                        crate::prefs::update(cx, |p| p.quick.toggle(q));
                        cx.notify();
                    })),
                );
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex().flex_col().child(up).child(down))
                    .child(div().w(px(18.)).text_color(l.text3()).child(format!("{}", i + 1)))
                    .child(div().flex_1().child(row(quick_name(q), Some(quick_hint(q)), control, l))),
            );
        }
        let lc = p.lifecycle;
        let stepper = |id: &'static str,
                       value: u16,
                       unit: &'static str,
                       range: std::ops::RangeInclusive<u16>,
                       on: bool,
                       set: fn(&mut postman_core::prefs::Lifecycle, u16)| {
            let step = |sid: &'static str, g: char, to: u16, enabled: bool| {
                div()
                    .id(sid)
                    .size(px(24.))
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
                            crate::prefs::update(cx, |p| set(&mut p.lifecycle, to));
                            cx.notify();
                        }))
                    })
            };
            let minus = step(id, '\u{E738}', value.saturating_sub(1), on && value > *range.start());
            let plus_id: &'static str = match id {
                "settings-wait-minus" => "settings-wait-plus",
                "settings-news-minus" => "settings-news-plus",
                _ => "settings-codes-plus",
            };
            let plus = step(plus_id, '\u{E710}', value.saturating_add(1), on && value < *range.end());
            div()
                .flex()
                .items_center()
                .gap_2()
                .when(!on, |d| d.text_color(l.text3()))
                .child(minus)
                .child(div().w(px(54.)).flex().justify_center().child(tr!(unit, n = value)))
                .child(plus)
        };
        let wait = stepper("settings-wait-minus", lc.wait_reply_days, "settings-days", DAYS, lc.wait_reply, |l, v| l.wait_reply_days = v);
        let news = stepper("settings-news-minus", lc.archive_news_days, "settings-days", DAYS, lc.archive_news, |l, v| l.archive_news_days = v);
        let codes = stepper("settings-codes-minus", lc.expire_codes_hours, "settings-hours", HOURS, lc.expire_codes, |l, v| l.expire_codes_hours = v);
        let rule = |i: usize, title: String, hint: String, on: bool, flip: fn(&mut postman_core::prefs::Lifecycle), extra: Option<Div>| {
            let control = div().flex().items_center().gap_4().children(extra).child(toggle(("settings-life-toggle", i), on, l).on_click(
                cx.listener(move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| flip(&mut p.lifecycle));
                    cx.notify();
                }),
            ));
            row(title, Some(hint), control, l)
        };
        // Куда уходит письмо в срок «Исчезнет» (S5-04): свой срок и коды подтверждения.
        use postman_core::prefs::ExpireTo;
        let target = |i: usize, to: ExpireTo, label: String| {
            seg(("settings-expire-to", i), label, lc.expire_to == to, true, l).on_click(cx.listener(move |_, _, _, cx| {
                crate::prefs::update(cx, |p| p.lifecycle.expire_to = to);
                cx.notify();
            }))
        };
        let expire_to = seg_group(
            [target(0, ExpireTo::Archive, tr!("lifecycle-settings-to-archive")), target(1, ExpireTo::Trash, tr!("lifecycle-settings-to-trash"))],
            l,
        );
        let rules = vec![
            rule(0, tr!("settings-life-wait"), tr!("settings-life-wait-hint"), lc.wait_reply, |l| l.wait_reply = !l.wait_reply, Some(wait)),
            row(tr!("lifecycle-settings-remind"), Some(tr!("lifecycle-settings-remind-hint")), div(), l),
            rule(1, tr!("settings-life-news"), tr!("settings-life-news-hint"), lc.archive_news, |l| l.archive_news = !l.archive_news, Some(news)),
            rule(2, tr!("settings-life-codes"), tr!("settings-life-codes-hint"), lc.expire_codes, |l| l.expire_codes = !l.expire_codes, Some(codes)),
            row(tr!("lifecycle-settings-expire-to"), Some(tr!("lifecycle-settings-expire-to-hint")), expire_to, l),
            rule(3, tr!("settings-life-overdue"), tr!("settings-life-overdue-hint"), lc.overdue_first, |l| l.overdue_first = !l.overdue_first, None),
        ];
        div()
            .flex()
            .flex_col()
            .child(group(tr!("settings-quick-group"), l))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-quick-note")))
            .child(list)
            .child(group(tr!("settings-life-group"), l))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-life-note")))
            .children(rules)
    }

    fn reading(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = crate::prefs::get(cx).reading;
        let mut cards = div().flex().gap_3();
        for (i, c) in Conversation::ALL.into_iter().enumerate() {
            let on = p.conversation == c;
            cards =
                cards.child(
                    div()
                        .id(("settings-conv", i))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_3()
                        .rounded(l.px(Metric::RadiusOverlay))
                        .cursor_pointer()
                        .border_1()
                        .when(on, |d| d.border_color(l.accent()).bg(l.picked()))
                        .when(!on, |d| d.border_color(l.stroke()).hover(|d| d.bg(l.hover())))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(radio(on, l))
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(conversation_name(c))),
                        )
                        .child(div().text_size(px(12.)).text_color(l.text2()).child(conversation_hint(c)))
                        .when(c == Conversation::Feed, |d| {
                            d.child(div().flex().child(
                                div().px_1p5().rounded_sm().bg(l.fill()).text_size(px(11.)).text_color(l.text2()).child(tr!("settings-default")),
                            ))
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            crate::prefs::update(cx, |p| p.reading.conversation = c);
                            cx.notify();
                        })),
                );
        }
        let mut folders = div().flex().flex_col();
        for (fi, (id, name)) in self.info.folders.iter().enumerate() {
            let own = p.folders.get(id).copied();
            let mut items = Vec::new();
            let id_all = id.clone();
            items.push(seg(("settings-folder-all", fi), tr!("settings-folder-inherit"), own.is_none(), true, l).on_click(cx.listener(
                move |_, _, _, cx| {
                    let id = id_all.clone();
                    crate::prefs::update(cx, |p| p.reading.set_folder(&id, None));
                    cx.notify();
                },
            )));
            for (ci, c) in Conversation::ALL.into_iter().enumerate() {
                let id = id.clone();
                items.push(seg(("settings-folder-view", fi * 4 + ci), conversation_short(c), own == Some(c), true, l).on_click(cx.listener(
                    move |_, _, _, cx| {
                        let id = id.clone();
                        crate::prefs::update(cx, |p| p.reading.set_folder(&id, Some(c)));
                        cx.notify();
                    },
                )));
            }
            folders = folders.child(row(name.clone(), None, seg_group(items, l), l));
        }
        let panes: Vec<Stateful<Div>> = Pane::ALL
            .into_iter()
            .enumerate()
            .map(|(i, pane)| {
                seg(("settings-pane", i), pane_name(pane), p.pane == pane, true, l).on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.reading.pane = pane);
                    cx.notify();
                }))
            })
            .collect();
        let images = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(l.px(Metric::RadiusControl))
            .bg(l.fill_passive())
            .child(icon('\u{E72E}', 12.).text_color(l.text2()))
            .child(tr!("settings-images-never"));
        let warn = p.warn_link_mismatch;
        div()
            .flex()
            .flex_col()
            .child(group(tr!("settings-conv-group"), l))
            .child(cards)
            .when(!self.info.folders.is_empty(), |d| {
                d.child(group(tr!("settings-conv-folders"), l))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-conv-folders-hint")))
                    .child(folders)
            })
            .child(group(tr!("settings-pane-group"), l))
            .child(row(tr!("settings-pane"), Some(tr!("settings-pane-hint")), seg_group(panes, l), l))
            // Приглашение (S5-24): карточка над текстом письма или колонка справа.
            .child(row(
                tr!("settings-invite-side"),
                Some(tr!("settings-invite-side-hint")),
                toggle("settings-invite-side-toggle", p.invite_side, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.reading.invite_side = !p.reading.invite_side);
                    cx.notify();
                })),
                l,
            ))
            .child(group(tr!("settings-external-group"), l))
            .child(row(tr!("settings-images"), Some(tr!("settings-images-hint")), images, l))
            .child(row(
                tr!("settings-link-mismatch"),
                Some(tr!("settings-link-mismatch-hint")),
                toggle("settings-link-toggle", warn, l).on_click(cx.listener(|_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.reading.warn_link_mismatch = !p.reading.warn_link_mismatch);
                    cx.notify();
                })),
                l,
            ))
    }

    /// Раздел: содержимое и карточки сбоку.
    fn content(&mut self, l: &Look, window: &mut Window, cx: &mut Context<Self>) -> (Option<Stateful<Div>>, AnyElement, Vec<Div>) {
        match self.section {
            Section::General => (None, self.general(l, cx).into_any_element(), Vec::new()),
            Section::Quick => {
                let aside = vec![aside_card(tr!("settings-quick-aside"), vec![tr!("settings-quick-aside-1"), tr!("settings-quick-aside-2")], l)];
                (None, self.quick(l, cx).into_any_element(), aside)
            }
            Section::Reading => {
                let aside =
                    vec![aside_card(tr!("settings-reading-aside"), vec![tr!("settings-reading-aside-1"), tr!("settings-reading-aside-2")], l)];
                (None, self.reading(l, cx).into_any_element(), aside)
            }
            Section::Calendar => self.calendar(l, cx),
            Section::Security => (None, self.security(l, cx).into_any_element(), Vec::new()),
            Section::Look => (None, self.look(l, cx).into_any_element(), Vec::new()),
            Section::Categories => self.categories(l, window, cx),
            Section::Keys => self.keyboard(l, cx),
            Section::Signatures => self.signatures(l, window, cx),
            Section::Accounts => self.accounts(l, cx),
            Section::Rules | Section::Vacation => {
                let panel = if self.section == Section::Rules { self.rules.clone() } else { self.vacation.clone() };
                let body = match panel {
                    Some(v) => v.into_any_element(),
                    None => div().text_color(l.text2()).child(tr!("settings-loading")).into_any_element(),
                };
                (None, body, Vec::new())
            }
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let (action, body, aside) = self.content(&l, window, cx);
        // Правила и автоответы — свои панели со своей прокруткой и отступами.
        let embedded = matches!(self.section, Section::Rules | Section::Vacation);
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(l.panel())
            .rounded_t(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_b_0()
            .border_color(l.c(Role::StrokeCard))
            .child(self.header(&l, action))
            .child(if embedded {
                div().flex_1().min_h_0().flex().child(body)
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(div().id("settings-content").size_full().overflow_y_scroll().track_scroll(&self.scroll).px_6().pb_6().child(body))
            });
        div()
            .size_full()
            .flex()
            .gap_2()
            .text_size(px(13.))
            .child(self.nav(&l, cx))
            .child(main)
            .when(!aside.is_empty(), |d| d.child(div().w(px(300.)).flex_none().flex().flex_col().gap_2().children(aside)))
    }
}

// --- названия ---

pub(crate) fn quick_name(q: Quick) -> String {
    tr!(&format!("settings-quick-{}", q.id()))
}

fn quick_hint(q: Quick) -> String {
    tr!(&format!("settings-quick-{}-hint", q.id()))
}

fn conversation_key(c: Conversation) -> &'static str {
    match c {
        Conversation::Feed => "feed",
        Conversation::Stack => "stack",
        Conversation::Outlook => "outlook",
    }
}

fn conversation_name(c: Conversation) -> String {
    tr!(&format!("settings-conv-{}", conversation_key(c)))
}

fn conversation_short(c: Conversation) -> String {
    tr!(&format!("settings-conv-{}-short", conversation_key(c)))
}

fn conversation_hint(c: Conversation) -> String {
    tr!(&format!("settings-conv-{}-hint", conversation_key(c)))
}

fn pane_name(p: Pane) -> String {
    match p {
        Pane::Right => tr!("settings-pane-right"),
        Pane::Bottom => tr!("settings-pane-bottom"),
        Pane::Hidden => tr!("settings-pane-hidden"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Section, found, matches};

    /// Поиск разделов: слова запроса в названии или ключевых словах, регистр и «ё» не важны.
    #[test]
    fn section_search() {
        assert!(matches("", "Общие", "тема плотность"));
        assert!(matches("тЕма", "Общие", "тема плотность язык"));
        assert!(matches("плот общ", "Общие", "тема плотность"));
        assert!(!matches("пароль", "Общие", "тема плотность"));
        assert!(matches("ещё", "Клавиатура", "еще сочетания"), "ё = е");
        assert!(matches("hotkeys", "Keyboard", "shortcuts hotkeys keys"));
        assert!(!matches("theme keys", "Keyboard", "shortcuts hotkeys keys"), "все слова запроса");
    }

    #[test]
    fn sections_without_the_server_feature_are_hidden() {
        use postman_core::model::{Feature, Features};
        assert_eq!(found("", Features::ALL), Section::ALL.to_vec());
        let mail = Features::NONE;
        let shown = found("", mail);
        assert!(!shown.contains(&Section::Rules) && !shown.contains(&Section::Vacation) && !shown.contains(&Section::Calendar));
        assert!(shown.contains(&Section::Accounts) && shown.contains(&Section::Categories));
        assert!(found("", mail.with(Feature::Rules, true)).contains(&Section::Rules));
    }

    /// У каждого раздела — название и ключевые слова в обоих языках (`settings.ftl`).
    #[test]
    fn every_section_has_texts() {
        for (lang, ftl) in [("ru", include_str!("../i18n/ru/settings.ftl")), ("en", include_str!("../i18n/en/settings.ftl"))] {
            for s in Section::ALL {
                for key in [format!("settings-nav-{}", s.key()), format!("settings-nav-{}-keywords", s.key()), format!("settings-hint-{}", s.key())] {
                    assert!(ftl.lines().any(|l| l.starts_with(&format!("{key} ="))), "{lang}: нет {key}");
                }
            }
        }
    }
}
