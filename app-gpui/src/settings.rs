//! Вкладка «Настройки» в экране почты (S5-09, `settings_tab.rs`) и настройки ящика на сервере: автоответ (раздел
//! и полоса), заполненность ящика в строке состояния и тост при > 90 % (S4-05), правила почты (S4-07, `rules.rs`).
//! Сервер — команды `sync::Command::Settings` и `Command::Categories`, ответы — `sync::Event::Settings`.

use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::{Level as Toast, ToastAction};
use postman_core::autoreply::{Audience, AutoReply, Text};
use postman_core::categories;
use postman_core::labels::Categories;
use postman_core::rules::{Action, Field, Rule};
use postman_core::settings::{Area, AutoReplySave, Foreign, Reply, Request, RulesState, Usage, WARN_PERCENT};
use postman_core::sync::{Command, Event};
use postman_core::model::VacationResponse;

use crate::Mailer;
use crate::rules::{RulesEvent, RulesPanel};
use crate::settings_tab::{Info, Section, SettingsEvent, SettingsView};
use crate::signin::SignIn;
use crate::vacation::{self, BarEvent, VacationBar, VacationEvent, VacationPanel};

#[derive(Default)]
pub(crate) struct ServerSettings {
    /// Автоответ на сервере (`None` — ещё не пришёл или сервер его не знает).
    pub(crate) vacation: Option<VacationResponse>,
    /// Настройки автоответа Postman (свой скрипт `vacation`, S5-29); `None` — их нет: один текст сервера всем.
    pub(crate) autoreply: Option<AutoReply>,
    pub(crate) quota: Option<Usage>,
    /// Правила на сервере (`None` — ещё не пришли или сервер их не знает).
    pub(crate) rules: Option<RulesState>,
    /// Тост о заполненности уже показан (снова — после того, как место освободилось).
    quota_warned: bool,
    /// Вкладка «Настройки» открыта.
    pub(crate) view: Option<Entity<SettingsView>>,
    /// Разделы «Автоответы» и «Правила» — панели, которые слушает экран почты.
    pub(crate) vacation_panel: Option<Entity<VacationPanel>>,
    pub(crate) rules_panel: Option<Entity<RulesPanel>>,
    /// Карточка «Добавить аккаунт» в разделе «Аккаунты».
    pub(crate) add: Option<Entity<SignIn>>,
    /// Полоса «Автоответ включён» — пока он включён.
    pub(crate) bar: Option<Entity<VacationBar>>,
    _subscriptions: Vec<Subscription>,
    /// Подписки вкладки: уходят вместе с ней.
    _tab: Vec<Subscription>,
}

impl ServerSettings {
    /// «Ящик: 9,4 из 10 ГБ» для строки состояния; квоты нет — пусто.
    pub(crate) fn quota_text(&self) -> String {
        self.quota.map(quota_text).unwrap_or_default()
    }
}

/// Число с одним знаком после запятой («9,4» — разделитель из текстов языка; целое — без «,0»).
fn decimal(n: f64, sep: &str) -> String {
    let s = if (n - n.round()).abs() < 0.05 { format!("{}", n.round()) } else { format!("{n:.1}") };
    s.replace('.', sep)
}

/// Занято и предел в единице предела (МБ или ГБ), одним знаком: (занято, предел, единица).
pub(crate) fn usage_parts(u: Usage) -> (f64, f64, &'static str) {
    let (unit, limit) = limit_parts(u.limit);
    const MB: f64 = 1024.0 * 1024.0;
    let div = if unit == "mb" { MB } else { MB * 1024.0 };
    ((u.used as f64 / div * 10.0).round() / 10.0, limit, unit)
}

/// То же строками для текстов.
fn usage_text(u: Usage) -> (String, String, &'static str) {
    let sep = tr!("settings-decimal-sep");
    let (used, limit, unit) = usage_parts(u);
    (decimal(used, &sep), decimal(limit, &sep), unit)
}

/// Предел квоты: меньше гигабайта — в МБ (целые), иначе в ГБ с одним знаком.
pub(crate) fn limit_parts(bytes: u64) -> (&'static str, f64) {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb < 1024.0 { ("mb", mb.round()) } else { ("gb", (mb / 1024.0 * 10.0).round() / 10.0) }
}

fn quota_text(u: Usage) -> String {
    let (used, limit, unit) = usage_text(u);
    tr!("settings-quota", used = used, limit = limit, unit = unit)
}

/// Запрос «Очистить вложения…»: письма с вложениями больше 5 МБ старше года (макет «Главное окно: автоответ»).
pub(crate) const CLEANUP_QUERY: &str = "has:attachment larger:5M before:1y";

/// Образец автоответа Postman для демо и снимка `vacation`: свои тексты коллегам и внешним.
pub(crate) fn reply_sample() -> AutoReply {
    AutoReply {
        internal: Text {
            subject: tr!("vacation-sample-internal-subject"),
            body: tr!("vacation-sample-internal-body"),
        },
        external: Text {
            subject: tr!("vacation-sample-external-subject"),
            body: tr!("vacation-sample-external-body"),
        },
        ..AutoReply::default()
    }
}

impl Mailer {
    /// Что вкладке нужно знать об экране: учётная запись, общие ящики, папки, адреса отправителя.
    fn settings_info(&self, cx: &App) -> Info {
        let folders = self.folders.read(cx);
        let mut identities = self.compose.sender_addresses();
        if identities.is_empty()
            && let Some(me) = self.reading.read(cx).me.clone()
        {
            identities.push(me);
        }
        let of = |p: Option<&str>| {
            folders
                .accounts
                .iter()
                .filter(move |a| postman_cache::profile_of(Some(&a.id)) == p && Some(a.id.as_str()) != p)
                .map(|a| a.name.clone())
                .collect::<Vec<_>>()
        };
        Info {
            live: self.backend.is_live(),
            account: self.live_account().cloned(),
            status: self.live.status.clone(),
            shared: of(None),
            extra: if self.backend.is_live() {
                self.live.profile.names.iter().map(|(p, s)| (p.clone(), s.clone(), of(Some(p)))).collect()
            } else {
                // Демо (S7-03б): вторая учётная запись — из дерева папок.
                folders
                    .accounts
                    .iter()
                    .filter(|a| postman_cache::profile_of(Some(&a.id)) == Some(a.id.as_str()))
                    .map(|a| (a.id.clone(), crate::account::Settings { url: String::new(), user: a.name.clone() }, of(Some(&a.id))))
                    .collect()
            },
            folders: folders.of(None).map(|m| (m.id.clone(), m.name.clone())).collect(),
            identities,
            cache: self.account_cache(),
            sandbox: self.sandbox,
        }
    }

    /// Таблица категорий своей учётной записи (как в списке писем).
    fn own_categories(&self, cx: &App) -> Categories {
        self.list.read(cx).labels.for_account(None).cloned().unwrap_or_else(crate::labels::default_table)
    }

    /// `settings.open` (рейл, Ctrl+,): вкладка «Настройки» на разделе `section`; открыта — тот же раздел или новый.
    pub(crate) fn open_settings(&mut self, section: Option<Section>, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.open(crate::tabs::TabKind::Settings);
        let info = self.settings_info(cx);
        match self.settings.view.clone() {
            Some(view) => {
                view.update(cx, |v, cx| {
                    v.set_info(info, cx);
                    if let Some(s) = section {
                        v.select(s, window, cx);
                    }
                });
                // Панель раздела — сразу, не дожидаясь события вкладки.
                if let Some(s) = section {
                    self.section_opened(s, window, cx);
                }
            }
            None => {
                let cats = self.own_categories(cx);
                let section = section.unwrap_or(Section::General);
                let view = cx.new(|cx| SettingsView::new(section, info, cats, window, cx));
                let sub = cx.subscribe_in(&view, window, |this, _, ev: &SettingsEvent, window, cx| this.on_settings(ev, window, cx));
                self.settings._tab.push(sub);
                self.settings.view = Some(view);
                self.section_opened(section, window, cx);
            }
        }
        cx.notify();
    }

    /// `settings.vacation`: раздел «Автоответы».
    pub(crate) fn open_vacation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(Some(Section::Vacation), window, cx);
    }

    /// `settings.rules`: раздел «Правила»; живой ящик — правила с сервера заново.
    pub(crate) fn open_rules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(Some(Section::Rules), window, cx);
    }

    pub(crate) fn on_settings(&mut self, ev: &SettingsEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            SettingsEvent::Opened(s) => self.section_opened(*s, window, cx),
            SettingsEvent::Categories(table) => self.edit_categories(table.clone(), cx),
            SettingsEvent::Purge(keyword) => {
                let request = categories::Request::Purge { account: None, keyword: keyword.clone(), ticket: 0 };
                if !self.backend.send(Command::Categories(request)) {
                    self.not_sent(cx);
                }
            }
            SettingsEvent::AddAccount => {
                let panel = cx.new(|cx| SignIn::new(None, false, window, cx));
                self.watch_signin(&panel, window, cx);
                self.settings.add = Some(panel.clone());
                if let Some(v) = &self.settings.view {
                    v.update(cx, |v, cx| {
                        v.add = Some(panel.into());
                        cx.notify();
                    });
                }
            }
            SettingsEvent::AccountPrefs(p) => self.compose.prefs = p.clone(),
            SettingsEvent::RemoveAccount => {
                self.close_settings(window, cx);
                self.remove_account(window, cx);
            }
            SettingsEvent::RemoveExtra(profile) => self.remove_extra(profile.clone(), cx),
        }
    }

    /// Раздел открыт: правилам и автоответам — их панели (правила — заново с сервера).
    fn section_opened(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        match section {
            Section::Vacation if self.settings.vacation_panel.is_none() => {
                let live = self.backend.is_live();
                let state = self.settings.vacation.clone();
                let reply = self.settings.autoreply.clone();
                let warning = self.foreign_warning();
                let me = self.vacation_me(cx);
                let panel = cx.new(|cx| {
                    let mut p = VacationPanel::new(state.as_ref(), reply.as_ref(), me, live, window, cx);
                    p.warning = warning;
                    p
                });
                let sub = cx.subscribe_in(&panel, window, |this, _, ev: &VacationEvent, _, cx| match ev {
                    VacationEvent::Save(v, reply) => this.save_autoreply(v.clone(), reply.clone(), cx),
                });
                self.settings._tab.push(sub);
                self.settings.vacation_panel = Some(panel.clone());
                self.give_panel(section, panel.into(), cx);
            }
            Section::Rules if self.settings.rules_panel.is_none() => {
                let live = self.backend.is_live();
                // Правила (Sieve) — своего ящика: папки общих ящиков (S4-06) в них не выбрать.
                let mailboxes: Vec<(String, String)> =
                    self.folders.read(cx).of(None).map(|m| (m.id.clone(), m.name.clone())).collect();
                let state = if live { self.settings.rules.clone() } else { Some(rules_sample(&mailboxes)) };
                if live && !self.backend.send(Command::Settings(Request::Rules)) {
                    self.sync_stopped(cx);
                }
                // Категории своего ящика (S5-26): действие «Категория».
                let categories: Vec<crate::rules::CategoryChoice> = self
                    .list
                    .read(cx)
                    .labels
                    .for_account(None)
                    .cloned()
                    .unwrap_or_else(crate::labels::default_table)
                    .visible()
                    .map(|c| (c.keyword.clone(), c.name.clone(), c.shown_color()))
                    .collect();
                let panel = cx.new(|cx| RulesPanel::new(state, mailboxes, categories, live, window, cx));
                let sub = cx.subscribe_in(&panel, window, |this, _, ev: &RulesEvent, _, cx| match ev {
                    RulesEvent::Save { rules, replace } => this.save_rules(rules.clone(), *replace, cx),
                });
                self.settings._tab.push(sub);
                self.settings.rules_panel = Some(panel.clone());
                self.give_panel(section, panel.into(), cx);
            }
            _ => {}
        }
    }

    fn give_panel(&mut self, section: Section, panel: AnyView, cx: &mut Context<Self>) {
        let Some(v) = &self.settings.view else { return };
        v.update(cx, |v, cx| {
            match section {
                Section::Vacation => v.vacation = Some(panel),
                _ => v.rules = Some(panel),
            }
            cx.notify();
        });
    }

    /// Правка таблицы категорий в настройках: в список и локальную копию сразу, на сервер — сведением (FileNode).
    fn edit_categories(&mut self, table: Categories, cx: &mut Context<Self>) {
        let mut book = self.list.read(cx).labels.clone();
        book.edit(None, table.clone());
        self.save_labels(&book, cx);
        self.list.update(cx, |l, cx| l.set_labels(book, cx));
        if self.backend.is_live() && !self.backend.send(Command::Categories(categories::Request::Save { account: None, table })) {
            self.sync_stopped(cx);
        }
    }

    /// Таблица категорий изменилась (сервер): вкладке — новую.
    pub(crate) fn categories_changed(&mut self, cx: &mut Context<Self>) {
        let cats = self.own_categories(cx);
        if let Some(v) = &self.settings.view {
            v.update(cx, |v, cx| v.set_categories(cats, cx));
        }
    }

    /// Вход подтверждён сервером: карточка добавления в настройках больше не нужна.
    pub(crate) fn signed_in(&mut self, cx: &mut Context<Self>) {
        self.settings.add = None;
        let info = self.settings_info(cx);
        if let Some(v) = &self.settings.view {
            v.update(cx, |v, cx| {
                v.add = None;
                v.set_info(info, cx);
            });
        }
    }

    /// Учётные записи изменились (S7-03б: добавлена или удалена дополнительная): вкладке — новые сведения.
    pub(crate) fn refresh_settings_info(&mut self, cx: &mut Context<Self>) {
        let info = self.settings_info(cx);
        if let Some(v) = &self.settings.view {
            v.update(cx, |v, cx| v.set_info(info, cx));
        }
    }

    /// «Отмена» в карточке добавления.
    pub(crate) fn cancel_add_account(&mut self, cx: &mut Context<Self>) {
        let panel = self.settings.add.take();
        // Вход с этой карточки уже показан на месте экрана почты — убрать и там.
        if panel.is_some() && self.live.signin.as_ref().map(|p| p.entity_id()) == panel.as_ref().map(|p| p.entity_id()) {
            self.live.signin = None;
        }
        if let Some(v) = &self.settings.view {
            v.update(cx, |v, cx| {
                v.add = None;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Esc во вкладке: сначала — запись клавиши или правка на месте; `false` — закрывать вкладку.
    pub(crate) fn settings_escape(&mut self, cx: &mut Context<Self>) -> bool {
        match &self.settings.view {
            Some(v) if self.tabs.is_active(crate::tabs::TabKind::Settings) => v.update(cx, |v, cx| v.escape(cx)),
            _ => false,
        }
    }

    /// Закрыть вкладку «Настройки»; `false` — её не было (Esc идёт дальше).
    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let open = self.settings.view.take().is_some();
        self.settings.vacation_panel = None;
        self.settings.rules_panel = None;
        self.settings.add = None;
        self.settings._tab.clear();
        self.tabs.close_kind(crate::tabs::TabKind::Settings);
        if open {
            // Клавиши — снова экрану почты.
            window.focus(&self.focus, cx);
            cx.notify();
        }
        open
    }

    fn save_vacation(&mut self, v: VacationResponse, cx: &mut Context<Self>) {
        if self.backend.send(Command::Settings(Request::SetVacation(v))) {
            return;
        }
        self.not_sent(cx);
        if let Some(p) = &self.settings.vacation_panel {
            p.update(cx, |p, cx| p.not_saved(cx));
        }
    }

    /// Мой адрес и имя — предпросмотр автоответа.
    fn vacation_me(&self, cx: &App) -> vacation::Me {
        match self.compose.me() {
            Some((a, _)) => vacation::Me { name: a.name.clone(), email: a.email.clone() },
            None => {
                let email = self.reading.read(cx).me.clone().unwrap_or_else(|| postman_demo::ME.to_owned());
                vacation::Me { name: String::new(), email }
            }
        }
    }

    /// Кому отвечать: свои домены и адреса, адреса моих контактов.
    fn audience(&self, cx: &App) -> Audience {
        let mine = self.compose.own_addresses();
        Audience { domains: Audience::domains_of(&mine), contacts: self.contacts.read(cx).personal_emails(), mine }
    }

    /// Автоответ Postman (S5-29): `VacationResponse`, свой скрипт и «Вне офиса».
    fn save_autoreply(&mut self, v: VacationResponse, reply: AutoReply, cx: &mut Context<Self>) {
        let save = AutoReplySave {
            vacation: v,
            reply,
            audience: self.audience(cx),
            ooo_title: tr!("vacation-ooo-title"),
            zone: crate::prefs::get(cx).calendar.zone.unwrap_or_default(),
        };
        if self.backend.send(Command::Settings(Request::SetAutoReply(Box::new(save)))) {
            return;
        }
        self.not_sent(cx);
        if let Some(p) = &self.settings.vacation_panel {
            p.update(cx, |p, cx| p.not_saved(cx));
        }
    }

    fn save_rules(&mut self, rules: Vec<Rule>, replace: bool, cx: &mut Context<Self>) {
        if self.backend.send(Command::Settings(Request::SaveRules { rules, replace })) {
            return;
        }
        self.not_sent(cx);
        if let Some(p) = &self.settings.rules_panel {
            p.update(cx, |p, cx| p.not_saved(cx));
        }
    }

    /// Команда не ушла: демо — «нужен сервер», живой ящик — синхронизация остановилась.
    fn not_sent(&mut self, cx: &mut Context<Self>) {
        if self.backend.is_live() { self.sync_stopped(cx) } else { self.needs_server(cx) }
    }

    /// «Выключить» на полосе: тот же автоответ, выключенный; тост — «Отменить» (включить снова).
    fn vacation_off(&mut self, cx: &mut Context<Self>) {
        self.vacation_switch(false, cx);
        let mailer = cx.entity().downgrade();
        let action = ToastAction::new(tr!("vacation-undo"), move |_, cx| {
            let _ = mailer.update(cx, |m, cx| m.vacation_switch(true, cx));
        });
        self.toasts.update(cx, |t, cx| t.push_action(Toast::Info, tr!("vacation-turned-off"), action, cx));
    }

    /// Включить или выключить автоответ, как он есть на сервере.
    fn vacation_switch(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(v) = self.settings.vacation.clone() else { return };
        let v = VacationResponse { is_enabled: on, ..v };
        match self.settings.autoreply.clone() {
            Some(reply) => self.save_autoreply(v, reply, cx),
            None => self.save_vacation(v, cx),
        }
    }

    /// Активный скрипт сервера не из Postman: включение автоответа его выключит (`docs/STALWART.md`).
    fn foreign_warning(&self) -> Option<String> {
        let name = self.settings.rules.as_ref()?.active_foreign()?;
        Some(tr!("vacation-foreign-warning", name = name.to_owned()))
    }

    /// Событие синхронизации — своё берём (`Event::Settings`).
    pub(crate) fn on_settings_sync(&mut self, ev: &Event, cx: &mut Context<Self>) {
        let Event::Settings(reply) = ev else { return };
        let vacation = self.settings.vacation_panel.clone();
        let rules = self.settings.rules_panel.clone();
        match reply {
            Reply::Vacation(v) => {
                let reply = self.settings.autoreply.clone();
                if let Some(p) = &vacation {
                    p.update(cx, |p, cx| p.server_state(v, reply.as_ref(), cx));
                }
                self.set_vacation_state(Some(v.clone()), cx);
            }
            Reply::AutoReply(reply) => {
                self.settings.autoreply = reply.clone();
                if let (Some(p), Some(v)) = (&vacation, self.settings.vacation.clone()) {
                    p.update(cx, |p, cx| p.server_state(&v, reply.as_ref(), cx));
                }
                let v = self.settings.vacation.clone();
                self.set_vacation_state(v, cx);
            }
            Reply::VacationSaved(v) => {
                let reply = self.settings.autoreply.clone();
                match &vacation {
                    Some(p) => p.update(cx, |p, cx| p.saved(v, reply.as_ref(), cx)),
                    None => {
                        let text = if v.is_enabled { tr!("vacation-saved") } else { tr!("vacation-saved-off") };
                        self.toasts.update(cx, |t, cx| t.push(Toast::Success, text, cx));
                    }
                }
                self.set_vacation_state(Some(v.clone()), cx);
            }
            Reply::Quota(usage) => self.set_quota(*usage, cx),
            Reply::Rules(s) => self.set_rules_state(s.clone(), false, cx),
            Reply::RulesSaved(s) => self.set_rules_state(s.clone(), true, cx),
            Reply::RulesInvalid(e) => {
                tracing::warn!("rules: server rejected the script: {e}");
                let text = tr!("rules-invalid", error = e.clone());
                self.rules_problem(text, cx);
            }
            Reply::RulesConflict { name, overwrite } => {
                let (name, overwrite) = (name.clone(), *overwrite);
                if let Some(p) = &rules {
                    p.update(cx, |p, cx| {
                        let pending = p.pending().cloned().unwrap_or_default();
                        p.conflict(name, overwrite, pending, cx)
                    });
                }
            }
            Reply::Unsupported(Area::Vacation) => {
                if let Some(p) = &vacation {
                    p.update(cx, |p, cx| p.failed(tr!("vacation-unsupported"), cx));
                }
            }
            Reply::Unsupported(Area::Rules) => {
                if let Some(p) = &rules {
                    p.update(cx, |p, cx| p.failed(tr!("rules-unsupported"), cx));
                }
            }
            Reply::Unsupported(Area::Quota) => {}
            Reply::Failed { area: Area::Vacation, error } => {
                tracing::warn!("vacation: {error}");
                let text = tr!("vacation-failed", error = error.clone());
                match &vacation {
                    Some(p) => p.update(cx, |p, cx| p.failed(text, cx)),
                    None => self.toasts.update(cx, |t, cx| t.push(Toast::Error, text, cx)),
                }
            }
            Reply::Failed { area: Area::Rules, error } => {
                tracing::warn!("rules: {error}");
                let text = tr!("rules-failed", error = error.clone());
                self.rules_problem(text, cx);
            }
            // Квота не прочиталась — строка состояния без неё; в журнал.
            Reply::Failed { area: Area::Quota, error } => tracing::warn!("quota: {error}"),
        }
    }

    fn set_rules_state(&mut self, state: RulesState, saved: bool, cx: &mut Context<Self>) {
        self.settings.rules = Some(state.clone());
        let warning = self.foreign_warning();
        if let Some(p) = &self.settings.vacation_panel {
            p.update(cx, |p, cx| {
                p.warning = warning;
                cx.notify();
            });
        }
        match &self.settings.rules_panel {
            Some(p) if saved => p.update(cx, |p, cx| p.saved(state, cx)),
            Some(p) => p.update(cx, |p, cx| p.server_state(state, cx)),
            None if saved => self.toasts.update(cx, |t, cx| t.push(Toast::Success, tr!("rules-saved"), cx)),
            None => {}
        }
    }

    /// Ошибка правил — в разделе «Правила» (редактор остаётся), без него — тостом.
    fn rules_problem(&mut self, text: String, cx: &mut Context<Self>) {
        match &self.settings.rules_panel {
            Some(p) => p.update(cx, |p, cx| p.failed(text, cx)),
            None => self.toasts.update(cx, |t, cx| t.push(Toast::Error, text, cx)),
        }
    }

    /// Автоответ на сервере изменился: полоса появляется, меняет текст или уходит.
    pub(crate) fn set_vacation_state(&mut self, v: Option<VacationResponse>, cx: &mut Context<Self>) {
        let reply = self.settings.autoreply.clone();
        let text =
            v.as_ref().filter(|v| v.is_enabled).map(|v| vacation::bar_text(v, reply.as_ref(), &TimeZone::system()));
        self.settings.vacation = v;
        match (text, &self.settings.bar) {
            (Some((until, reach)), Some(bar)) => bar.update(cx, |b, cx| {
                b.until = until;
                b.reach = reach;
                cx.notify();
            }),
            (Some((until, reach)), None) => {
                let bar = cx.new(|_| VacationBar { until, reach });
                self.settings
                    ._subscriptions
                    .push(cx.subscribe(&bar, |this, _, BarEvent::TurnOff, cx| this.vacation_off(cx)));
                self.settings.bar = Some(bar);
            }
            (None, _) => self.settings.bar = None,
        }
        cx.notify();
    }

    pub(crate) fn set_quota(&mut self, usage: Option<Usage>, cx: &mut Context<Self>) {
        self.settings.quota = usage;
        let percent = usage.map_or(0, |u| u.percent());
        if percent > WARN_PERCENT && !self.settings.quota_warned {
            self.settings.quota_warned = true;
            self.quota_toast(cx);
        } else if percent <= WARN_PERCENT {
            self.settings.quota_warned = false;
        }
        cx.notify();
    }
}

impl Mailer {
    /// Тост о заполненности с «Очистить вложения…» (S5-29): поиск «Везде» больших писем с вложениями.
    pub(crate) fn quota_toast(&mut self, cx: &mut Context<Self>) {
        let Some(u) = self.settings.quota else { return };
        let (used, limit, unit) = usage_text(u);
        let text = tr!("settings-quota-warn", percent = u.percent(), used = used, limit = limit, unit = unit);
        let mailer = cx.entity().downgrade();
        let action = ToastAction::new(tr!("settings-quota-clean"), move |window, cx| {
            let _ = mailer.update(cx, |m, cx| m.clean_attachments(window, cx));
        });
        self.toasts.update(cx, |t, cx| t.push_action(Toast::Warning, text, action, cx));
    }

    /// «Очистить вложения…»: «Почта», поиск «Везде» по [`CLEANUP_QUERY`].
    pub(crate) fn clean_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_section(crate::Section::Mail, cx);
        self.bar.update(cx, |b, _| b.scope = crate::bar::Scope::All);
        self.search.update(cx, |s, cx| s.set_value(CLEANUP_QUERY, window, cx));
        self.run_search(CLEANUP_QUERY, cx);
    }
}

/// Образец для снимка `vacation` в демо: включён на неделю с сегодняшнего дня.
pub(crate) fn screen_sample() -> VacationResponse {
    let tz = TimeZone::system();
    let today = jiff::Zoned::now().with_time_zone(tz.clone()).date();
    let at = |d: jiff::civil::Date| d.to_zoned(tz.clone()).ok().map(|z| z.timestamp().to_string());
    VacationResponse {
        is_enabled: true,
        from_date: at(today),
        to_date: today.checked_add(jiff::Span::new().days(8)).ok().and_then(at),
        subject: Some("Нет на месте".into()), // i18n-ok: образец автоответа
        text_body: Some("Здравствуйте! Я в отпуске до конца недели.\nПо срочным вопросам — Иван Петров.".into()), // i18n-ok: образец
        html_body: None,
    }
}

/// Образец для демо и снимка `rules`: правила по папкам демо-ящика и чужой скрипт сервера.
pub(crate) fn rules_sample(mailboxes: &[(String, String)]) -> RulesState {
    let (mailbox_id, name) = mailboxes.get(4).or(mailboxes.last()).cloned().unwrap_or_default();
    let rule = |enabled, field, value: &str, action| Rule { enabled, field, value: value.into(), action };
    RulesState {
        rules: vec![
            rule(true, Field::Subject, "Тендер", Action::Category(postman_core::labels::category_keyword("tenders"))), // i18n-ok: образец правила (S5-26, снимок `rules` — его правка)
            rule(true, Field::Subject, "Отчёт", Action::Move { mailbox_id, name }), // i18n-ok: образец правила
            rule(true, Field::From, "ci@corp.example", Action::MarkRead),
            rule(true, Field::To, "director@corp.example", Action::Flag),
            rule(false, Field::Subject, "Розыгрыш", Action::Discard), // i18n-ok: образец правила
        ],
        active: true,
        foreign: vec![Foreign {
            name: "old-filters".into(),
            text: "require \"fileinto\";\nif address :is \"from\" \"news@example.com\" {\n    fileinto \"News\";\n}"
                .into(),
            active: false,
        }],
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "settings", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &settings::Open, window, cx| this.open_settings(None, window, cx)))
        .on_action(cx.listener(|this, _: &settings::Vacation, window, cx| this.open_vacation(window, cx)))
        .on_action(cx.listener(|this, _: &settings::Rules, window, cx| this.open_rules(window, cx)))
        .on_action(cx.listener(|this, _: &settings::Calendar, window, cx| {
            this.open_settings(Some(crate::settings_tab::Section::Calendar), window, cx)
        }))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[
    ("ru", include_str!("../i18n/ru/settings.ftl")),
    ("en", include_str!("../i18n/en/settings.ftl")),
];

/// Вкладка «Настройки» (S5-09); автоответы и правила (S4-05, S4-07) — её разделы, в Outlook — меню «Файл».
// Имя — область действий (`settings::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod settings {
    gpui_kit::actions!(settings, [Open, Vacation, Rules, Calendar]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{cmd};
    &[
        // Не Outlook (там «Файл → Параметры»): Ctrl+, — как в новом Outlook и редакторах (S5-09).
        cmd("settings.open", &["ctrl-,"], None),
        // Outlook: «Файл → Автоматические ответы», без клавиши.
        cmd("settings.vacation", &[], None),
        // Outlook: «Файл → Правила и оповещения», без клавиши.
        cmd("settings.rules", &[], None),
        // Outlook: «Файл → Параметры → Календарь», без клавиши (S5-20).
        cmd("settings.calendar", &[], None),
    ]
};

/// Сцены `--screens` вкладки «Настройки» (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Scene};
    use crate::settings_tab::Section as S;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // «Настройки → Автоответ» (S4-05, S5-29; демо — образец со своими текстами коллегам и внешним).
        Scene::new("vacation", vacation, vacation_ready).live(),
        // Панель «Правила» (S4-07): демо — образец с редактором первого правила, живой ящик — правила с сервера.
        Scene::new("rules", rules, rules_ready).live(),
        // Вкладка «Настройки», раздел «Общие» (S5-09).
        Scene::new("settings-general", section, section_ready),
        // «Настройки → Безопасность» (S5-30).
        Scene::new("settings-security", section, section_ready),
        // «Настройки → Внешний вид» (S5-30).
        Scene::new("settings-look", section, section_ready),
        // «Настройки → Категории» с правкой новой категории: палитра с перечёркнутыми близкими цветами.
        Scene::new("settings-categories", categories, section_ready),
        // «Настройки → Клавиатура»: своё сочетание у одной команды и конфликт у другой.
        Scene::new("settings-keys", keys, section_ready),
        // «Настройки → Аккаунты» с карточкой «Добавить аккаунт» на месте.
        Scene::new("settings-accounts", accounts, section_ready),
        // «Настройки → Быстрые фильтры и сроки».
        Scene::new("settings-quick", section, section_ready),
        // «Настройки → Чтение».
        Scene::new("settings-reading", section, section_ready),
        // «Настройки → Календарь» (S5-20).
        Scene::new("settings-calendar", section, section_ready),
        // Тот же раздел, прокручен вниз: цвет свободного времени с образцом, календари коллег, приглашения.
        Scene::new("settings-calendar-free", calendar_free, section_ready),
        // «Настройки → Подписи и шаблоны» с образцом подписи и шаблона (демо — без файла).
        Scene::new("settings-sign", sign, section_ready),
    ];

    fn vacation(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        if !m.backend.is_live() {
            m.settings.autoreply = Some(super::reply_sample());
            m.set_vacation_state(Some(super::screen_sample()), cx);
            // Строка состояния — с заполненностью ящика: 12 % из 2 ГБ.
            let limit: u64 = 2 << 30;
            m.set_quota(Some(postman_core::settings::Usage { used: limit / 8, limit }), cx);
        }
        m.open_vacation(window, cx);
        Ok(())
    }

    fn vacation_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        // Живой ящик: автоответ прочитан с сервера (полоса — только если он включён).
        let state = if screens::live(m) { m.settings.vacation.is_some() } else { m.settings.bar.is_some() };
        screens::settings_on(m, S::Vacation, cx) && m.settings.vacation_panel.is_some() && state
    }

    fn rules(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.open_rules(window, cx);
        // Демо: редактор первого правила открыт — снимок показывает и список, и редактор.
        if let (false, Some(p)) = (m.backend.is_live(), m.settings.rules_panel.clone()) {
            p.update(cx, |p, cx| {
                let first = p.state.as_ref().and_then(|s| s.rules.first().cloned());
                if let Some(r) = first {
                    p.edit(crate::rules::Draft::of(0, &r), cx);
                }
            });
        }
        Ok(())
    }

    fn rules_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let live = screens::live(m);
        match &m.settings.rules_panel {
            Some(p) if screens::settings_on(m, S::Rules, cx) => {
                let p = p.read(cx);
                if live { p.state.is_some() } else { p.editor.is_some() }
            }
            _ => false,
        }
    }

    /// Раздел вкладки сцены.
    fn of(s: &Scene) -> S {
        match s.name {
            "settings-general" => S::General,
            "settings-security" => S::Security,
            "settings-look" => S::Look,
            "settings-categories" => S::Categories,
            "settings-keys" => S::Keys,
            "settings-quick" => S::Quick,
            "settings-reading" => S::Reading,
            "settings-calendar" | "settings-calendar-free" => S::Calendar,
            "settings-sign" => S::Signatures,
            _ => S::Accounts,
        }
    }

    fn section(s: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.open_settings(Some(of(s)), window, cx);
        Ok(())
    }

    fn section_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::settings_on(m, of(s), cx)
            && m.settings.view.as_ref().is_some_and(|v| {
                let v = v.read(cx);
                match s.name {
                    "settings-categories" => v.cat_edit.is_some(),
                    "settings-accounts" => v.add.is_some(),
                    "settings-sign" => !v.sign.prefs.signatures.is_empty(),
                    _ => true,
                }
            })
    }

    fn categories(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.open_settings(Some(S::Categories), window, cx);
        if let Some(v) = m.settings.view.clone() {
            v.update(cx, |v, cx| v.edit_category(None, window, cx));
        }
        Ok(())
    }

    fn keys(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Своё сочетание у «Флаг» и конфликт: «Сегодня в календаре» на Ctrl+2, как «Календарь»; схема «Одиночные
        // клавиши» (S5-17) со своей буквой у «Архив».
        crate::prefs::update(cx, |p| {
            p.keys.insert("mail.toggle_flag".into(), vec!["ctrl-g".into()]);
            p.keys.insert("calendar.today".into(), vec!["ctrl-2".into()]);
            p.key_scheme = postman_core::prefs::KeyScheme::Single;
            p.keys_single.insert("mail.archive".into(), vec!["y".into()]);
        });
        m.open_settings(Some(S::Keys), window, cx);
        Ok(())
    }

    fn accounts(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        crate::prefs::update(cx, |p| {
            p.keys.clear();
            p.keys_single.clear();
            p.key_scheme = postman_core::prefs::KeyScheme::Outlook;
        });
        m.open_settings(Some(S::Accounts), window, cx);
        m.on_settings(&crate::settings_tab::SettingsEvent::AddAccount, window, cx);
        Ok(())
    }

    fn calendar_free(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.open_settings(Some(S::Calendar), window, cx);
        if let Some(v) = m.settings.view.clone() {
            v.update(cx, |v, cx| {
                v.scroll.scroll_to_bottom();
                cx.notify();
            });
        }
        Ok(())
    }

    fn sign(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.open_settings(Some(S::Signatures), window, cx);
        if let Some(v) = m.settings.view.clone() {
            v.update(cx, |v, cx| {
                v.sign.prefs = crate::settings_sign::screen_sample(&v.info.identities);
                cx.notify();
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Usage, decimal, limit_parts, usage_parts};

    #[test]
    fn quota_limit_units() {
        assert_eq!(limit_parts(500 * 1024 * 1024), ("mb", 500.0));
        assert_eq!(limit_parts(2 * 1024 * 1024 * 1024), ("gb", 2.0));
        assert_eq!(limit_parts(1536 * 1024 * 1024), ("gb", 1.5));
        // «9,4 из 10 ГБ» (макет): занято — в той же единице, одним знаком; целое — без «,0».
        let gb = 1024 * 1024 * 1024u64;
        let u = Usage { used: gb * 94 / 10, limit: 10 * gb };
        assert_eq!(usage_parts(u), (9.4, 10.0, "gb"));
        assert_eq!((decimal(9.4, ","), decimal(10.0, ",")), ("9,4".to_string(), "10".to_string()));
        assert_eq!(usage_parts(Usage { used: 300 * 1024 * 1024, limit: 500 * 1024 * 1024 }), (300.0, 500.0, "mb"));
    }
}
