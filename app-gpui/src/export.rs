//! «Экспорт…» (S8-32, макет S8-D «Экспорт»): писем выделения, папки (с подпапками или без), учётки или всего — в
//! файлы популярных клиентов. Без модальных окон: выбор формата и охвата — полосой на месте полосы автоответа, каталог
//! — диалогом Windows (отмена диалога — полоса остаётся); ход — в строке состояния, щелчок — полоса с «Остановить»;
//! конец — тост с «Открыть папку».
//!
//! Задание ведёт поток ядра (`export::Exporter`, `migration::Core`): `Command::Export` → `Event::Export`. Форматы —
//! те, у которых есть писатель (`Format::available`): MSG и PST появятся в списке сами.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::ToastAction;
use pets_ui::theme::{Metric, Role};
use postman_core::export::{ExportId, Format, Scope, State};
use postman_core::sync::{Command, Home};

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::local::{LOCAL, home};
use crate::migration::{count_text, link, meter, strip};
use crate::roles::Look;

/// Значок экспорта (Segoe Fluent Icons «Export»).
pub(crate) const EXPORT: char = '\u{EDE1}';

/// Форматы в порядке списка (макет): имя и строка подсказки.
pub(crate) const FORMATS: [(Format, &str, &str); 5] = [
    (Format::Eml, "EML", "export-eml"),
    (Format::Mbox, "mbox", "export-mbox"),
    (Format::Maildir, "Maildir", "export-maildir"),
    (Format::Msg, "MSG", "export-msg"),
    (Format::Pst, "PST", "export-pst"),
];

/// Форматы, которые можно выбрать: есть писатель (PST — после S8-33б).
pub(crate) fn formats() -> impl Iterator<Item = (Format, &'static str, &'static str)> {
    FORMATS.into_iter().filter(|(f, ..)| f.available())
}

pub(crate) fn format_name(f: Format) -> &'static str {
    FORMATS.iter().find(|(x, ..)| *x == f).map_or("", |(_, n, _)| n)
}

/// Что выгружаем (ключи дерева и письма — как в интерфейсе).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum What {
    Letters(Vec<String>),
    Folder(String),
    Account(String),
    All,
}

/// Полоса выбора.
#[derive(Clone, Debug)]
pub(crate) struct Choose {
    pub(crate) what: What,
    /// Писем (для подписи).
    pub(crate) n: u64,
    /// Подпапок у папки (флажок «С подпапками (N)» — только когда они есть).
    pub(crate) subfolders: usize,
    pub(crate) with_subfolders: bool,
    pub(crate) format: Format,
    /// Список форматов открыт у этой точки.
    pub(crate) menu_at: Option<Point<Pixels>>,
}

/// Идущее задание.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub(crate) format: Format,
    pub(crate) dir: PathBuf,
    pub(crate) done: u64,
    pub(crate) total: u64,
    pub(crate) fetching: u64,
}

/// Экспорт на экране.
#[derive(Default)]
pub(crate) struct Exports {
    pub(crate) choose: Option<Choose>,
    pub(crate) runs: BTreeMap<ExportId, Run>,
    /// Задание, которое ушло в ядро, но номера ещё нет: формат и каталог для первого события.
    pub(crate) pending: Vec<(Format, PathBuf)>,
    /// Полоса хода открыта щелчком по строке состояния.
    pub(crate) open: bool,
    /// Формат по умолчанию — прошлый выбор (впервые — EML).
    pub(crate) last: Option<Format>,
}

/// Подпапки папки `key` в дереве (все уровни).
pub(crate) fn descendants(mailboxes: &[postman_cache::MailboxRow], key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut frontier = vec![key.to_owned()];
    while let Some(p) = frontier.pop() {
        for m in mailboxes.iter().filter(|m| m.parent_id.as_deref() == Some(p.as_str())) {
            if !out.contains(&m.id) && m.id != key {
                out.push(m.id.clone());
                frontier.push(m.id.clone());
            }
        }
    }
    out
}

/// Учётка ключа — `Home` ядра.
fn home_of(account: &str) -> Home {
    if account == LOCAL { Home::Local } else { Home::Account(account.to_owned()) }
}

/// Охват ядра (в фоне: письма и папки — по базе); письма без такого — пропуск, папки нет — `None`.
fn scope(cache: &postman_cache::Cache, what: &What, subfolders: bool, names: &[(String, String)]) -> Option<Scope> {
    Some(match what {
        What::Letters(ids) => Scope::Letters(ids.iter().filter_map(|k| postman_core::store::letter_of_key(cache, k).ok().flatten()).collect()),
        What::Folder(key) => Scope::Folder { id: postman_core::model::FolderId(crate::local::folder_id(cache, key)?.0), subfolders },
        What::Account(a) => Scope::Account(home_of(a)),
        What::All => Scope::All(names.iter().map(|(h, n)| (home_of(h), n.clone())).collect()),
    })
}

impl Mailer {
    /// «Экспорт…»: полоса выбора для `what`.
    pub(crate) fn export_ask(&mut self, what: What, cx: &mut Context<Self>) {
        let mailboxes = self.folders.read(cx).mailboxes.clone();
        let (n, subfolders) = match &what {
            What::Letters(ids) => (ids.len() as u64, 0),
            What::Folder(key) => {
                let total = mailboxes.iter().find(|m| &m.id == key).map_or(0, |m| u64::from(m.total));
                (total, descendants(&mailboxes, key).len())
            }
            What::Account(a) => (mailboxes.iter().filter(|m| home(&m.id) == a).map(|m| u64::from(m.total)).sum(), 0),
            What::All => (mailboxes.iter().map(|m| u64::from(m.total)).sum(), 0),
        };
        if matches!(&what, What::Letters(ids) if ids.is_empty()) {
            return self.toast(pets_ui::Level::Info, tr!("actions-select-letter"), cx);
        }
        let format = self.export.last.filter(|f| f.available()).unwrap_or(Format::Eml);
        self.export.choose = Some(Choose { what, n, subfolders, with_subfolders: subfolders > 0, format, menu_at: None });
        cx.notify();
    }

    /// `mail.export`: выделение, иначе открытое письмо; ничего — открытая папка.
    fn export_mail(&mut self, cx: &mut Context<Self>) {
        let ids = self.targets(cx);
        match (ids.is_empty(), self.list.read(cx).mailbox.clone()) {
            (false, _) => self.export_ask(What::Letters(ids), cx),
            (true, Some(f)) => self.export_ask(What::Folder(f), cx),
            _ => self.export_ask(What::Letters(Vec::new()), cx),
        }
    }

    /// Подпись полосы: «Экспорт «Учётка · Папка» · N писем».
    fn export_title(&self, c: &Choose, cx: &App) -> String {
        let n = c.n as i64;
        match &c.what {
            What::Letters(_) => tr!("export-what-selection", n = n, count = count_text(c.n)),
            What::Folder(key) => tr!("export-what-folder", folder = self.place_name(key, cx), n = n, count = count_text(c.n)),
            What::Account(a) => tr!("export-what-account", account = self.account_name(a, cx), n = n, count = count_text(c.n)),
            What::All => tr!("export-what-all", n = n, count = count_text(c.n)),
        }
    }

    /// «Выбрать папку и начать…»: каталог — диалогом, затем задание ядру. Отмена диалога — полоса остаётся.
    fn export_start(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.export.choose.clone() else { return };
        let names: Vec<(String, String)> = {
            let mut homes: Vec<String> = Vec::new();
            for m in &self.folders.read(cx).mailboxes {
                let h = home(&m.id).to_owned();
                if !homes.contains(&h) {
                    homes.push(h);
                }
            }
            homes.into_iter().map(|h| (h.clone(), self.account_name(&h, cx))).collect()
        };
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        let cache = self.cache.clone();
        cx.spawn(async move |this, cx| {
            let dir = match picked.await {
                Ok(Ok(Some(mut dirs))) if !dirs.is_empty() => dirs.swap_remove(0),
                _ => return,
            };
            let what = c.what.clone();
            let with = c.with_subfolders;
            let scope = cx.background_spawn(async move { scope(&cache, &what, with, &names) }).await;
            let _ = this.update(cx, |m, cx| {
                let Some(scope) = scope else { return };
                m.export.choose = None;
                m.export.last = Some(c.format);
                m.export.pending.push((c.format, dir.clone()));
                m.core_send(Command::Export { scope, format: c.format, dir }, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Ход и конец задания (`Event::Export`).
    pub(crate) fn on_export(&mut self, job: ExportId, done: u64, total: u64, state: State, cx: &mut Context<Self>) {
        if !self.export.runs.contains_key(&job) {
            let (format, dir) = if self.export.pending.is_empty() { (Format::Eml, PathBuf::new()) } else { self.export.pending.remove(0) };
            self.export.runs.insert(job, Run { format, dir, done, total, fetching: 0 });
        }
        let Some(run) = self.export.runs.get_mut(&job) else { return };
        run.done = done;
        run.total = total;
        match state {
            State::Running { fetching } => run.fetching = fetching,
            end => {
                let run = run.clone();
                self.export.runs.remove(&job);
                if self.export.runs.is_empty() {
                    self.export.open = false;
                }
                self.export_ended(&run, end, cx);
            }
        }
        cx.notify();
    }

    /// Тост в конце: итог и «Открыть папку».
    fn export_ended(&mut self, run: &Run, state: State, cx: &mut Context<Self>) {
        let (n, done, total) = (run.done as i64, count_text(run.done), count_text(run.total));
        let (level, text) = match state {
            State::Done { unfetched } if unfetched.is_empty() => {
                (pets_ui::Level::Success, format!("{}\n{} · {}", tr!("export-done", n = n, count = done), format_name(run.format), run.dir.display()))
            }
            State::Done { unfetched } => {
                let lines: Vec<String> = unfetched
                    .iter()
                    .map(|(h, k)| {
                        let server = match h {
                            Home::Local => tr!("local-group"),
                            Home::Account(a) => self.account_name(a, cx),
                        };
                        tr!("export-failed-fetch", n = *k as i64, count = count_text(*k), server = server)
                    })
                    .collect();
                (pets_ui::Level::Warning, format!("{}\n{}", tr!("export-done-partial", done = done, total = total), lines.join("\n")))
            }
            State::Stopped => {
                (pets_ui::Level::Info, format!("{}\n{}", tr!("export-stopped", done = done, total = total), tr!("export-stopped-detail")))
            }
            State::Failed(e) => (pets_ui::Level::Error, format!("{}\n{e}", tr!("export-stopped", done = done, total = total))),
            State::Running { .. } => return,
        };
        let dir = run.dir.clone();
        let action = ToastAction::new(tr!("export-open"), move |_, _| {
            if let Err(e) = crate::sys::shell::open(&dir.to_string_lossy()) {
                tracing::warn!("export folder: {e}");
            }
        });
        self.toasts.update(cx, |t, cx| t.push_action(level, text, action, cx));
    }

    /// Esc: полоса выбора, затем полоса хода. `false` — закрывать было нечего.
    pub(crate) fn export_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(c) = &mut self.export.choose {
            if c.menu_at.take().is_none() {
                self.export.choose = None;
            }
            cx.notify();
            return true;
        }
        if self.export.open {
            self.export.open = false;
            cx.notify();
            return true;
        }
        false
    }

    /// Полоса над списком: выбор формата и охвата или ход по щелчку.
    pub(crate) fn export_bar(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(c) = &self.export.choose {
            let picker = div()
                .id("export-format")
                .flex_none()
                .h(px(26.))
                .w(px(96.))
                .px(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .rounded(l.px(Metric::RadiusControl))
                .border_1()
                .border_color(l.stroke())
                .bg(l.panel())
                .cursor_pointer()
                .child(format_name(c.format))
                .child(icon(glyph::CHEVRON_DOWN, 10.).text_color(l.text2()))
                .on_click(cx.listener(|this, ev: &ClickEvent, _, cx| {
                    if let Some(c) = &mut this.export.choose {
                        c.menu_at = if c.menu_at.is_some() { None } else { Some(ev.position()) };
                    }
                    cx.notify();
                }));
            let sub = (c.subfolders > 0).then(|| {
                let on = c.with_subfolders;
                div()
                    .id("export-subfolders")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .child(crate::list::checkbox(on, l))
                    .child(tr!("export-subfolders", n = c.subfolders as i64))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(c) = &mut this.export.choose {
                            c.with_subfolders = !c.with_subfolders;
                        }
                        cx.notify();
                    }))
            });
            return Some(
                strip("export-choose-bar", EXPORT, l)
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(self.export_title(c, cx)))
                    .child(picker)
                    .children(sub)
                    .child(div().flex_1())
                    .child(link("export-start", tr!("export-start"), l, cx, |m, cx| m.export_start(cx)))
                    .child(link("export-cancel", tr!("export-cancel"), l, cx, |m, cx| {
                        m.export.choose = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        let (_, run) = self.export.runs.iter().next().filter(|_| self.export.open)?;
        let job = *self.export.runs.keys().next()?;
        let mut detail = format!("· {} {}", count_text(run.done), tr!("migration-of", total = count_text(run.total)));
        if run.fetching > 0 {
            detail = format!("{detail} · {}", tr!("export-fetching", n = count_text(run.fetching)));
        }
        Some(
            strip("export-run-bar", EXPORT, l)
                .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(tr!("export-running", format = format_name(run.format))))
                .child(div().flex_none().text_color(l.text2()).child(detail))
                .child(meter(run.done, run.total, 120., false, l))
                .child(div().flex_1())
                .child(link("export-stop", tr!("export-stop"), l, cx, move |m, cx| m.core_send(Command::StopExport { job }, cx)))
                .into_any_element(),
        )
    }

    /// Список форматов под кнопкой полосы.
    pub(crate) fn export_menu(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.export.choose.as_ref()?;
        let at = c.menu_at?;
        let items = formats().enumerate().map(|(i, (f, name, hint))| {
            let on = f == c.format;
            div()
                .id(("export-format-item", i))
                .px(px(10.))
                .py(px(6.))
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .when(on, |d| d.bg(l.c(Role::FillCursor)))
                .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                .child(div().child(name))
                .child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!(hint)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(c) = &mut this.export.choose {
                        c.format = f;
                        c.menu_at = None;
                    }
                    cx.notify();
                }))
        });
        let body = div()
            .id("export-format-menu")
            .occlude()
            .w(px(320.))
            .p(px(4.))
            .flex()
            .flex_col()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .shadow_lg()
            .text_size(px(13.))
            .text_color(l.text())
            .children(items)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if let Some(c) = &mut this.export.choose {
                    c.menu_at = None;
                }
                cx.notify();
            }));
        Some(
            deferred(anchored().position(at + point(px(0.), px(14.))).snap_to_window_with_margin(px(8.)).child(body))
                .with_priority(2)
                .into_any_element(),
        )
    }

    /// Ход в строке состояния: «Экспорт: N из M»; щелчок — полоса с «Остановить».
    pub(crate) fn export_status(&self, l: &Look, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let run = self.export.runs.values().next()?;
        Some(
            div()
                .id("export-status")
                .h(px(22.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .when(self.export.open, |d| d.bg(l.pressed()))
                .child(icon(EXPORT, 12.))
                .child(tr!("export-status", done = count_text(run.done), total = count_text(run.total)))
                .child(meter(run.done, run.total, 64., false, l))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.export.open = !this.export.open;
                    cx.notify();
                })),
        )
    }
}

/// Опись части (ADR 0005, `parts.rs`): команды, тексты и сцены экспорта.
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "export", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, reset: Some(scenes::reset) };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/export.ftl")), ("en", include_str!("../i18n/en/export.ftl"))];

/// Действия: «Экспорт…» писем (или открытой папки), учётки открытой папки, всего.
pub(crate) mod act {
    pub(crate) mod mail {
        gpui_kit::actions!(mail, [Export]);
    }
    pub(crate) mod account {
        gpui_kit::actions!(account, [Export]);
    }
    pub(crate) mod export {
        gpui_kit::actions!(export, [All]);
    }
}

const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAIL, cmd};
    &[cmd("mail.export", &[], MAIL), cmd("account.export", &[], MAIL), cmd("export.all", &[], MAIL)]
};

fn handlers(root: Div, cx: &mut Context<Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &act::mail::Export, _, cx| this.export_mail(cx)))
        .on_action(cx.listener(|this, _: &act::account::Export, _, cx| {
            let account = this.list.read(cx).mailbox.as_deref().map(|m| home(m).to_owned()).unwrap_or_default();
            this.export_ask(What::Account(account), cx);
        }))
        .on_action(cx.listener(|this, _: &act::export::All, _, cx| this.export_ask(What::All, cx)))
}

/// Сцены `--screens`: выбор формата у папки с подпапками; идущий экспорт с полосой хода.
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::Scene;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // «Экспорт «… · Проекты»»: формат — список под кнопкой, «С подпапками (N)».
        Scene::new("export-choose", choose, choose_ready),
        // Идёт экспорт в mbox: полоса хода (щелчок по строке состояния) и ход в строке состояния.
        Scene::new("export-running", running, running_ready),
    ];

    pub(crate) fn reset(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        m.export = Default::default();
        cx.notify();
    }

    fn choose(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let mailboxes = m.folders.read(cx).mailboxes.clone();
        // Своя папка с подпапками (в демо — «Проекты»).
        let with_subs = |f: &&postman_cache::MailboxRow| f.role.is_none() && !super::descendants(&mailboxes, &f.id).is_empty();
        let folder = mailboxes
            .iter()
            .filter(with_subs)
            .find(|f| postman_cache::account_of(&f.id).is_none())
            .or_else(|| mailboxes.iter().find(with_subs))
            .ok_or("no folder with subfolders")?;
        let id = folder.id.clone();
        m.open_mailbox(id.clone(), cx);
        m.export_ask(super::What::Folder(id), cx);
        if let Some(c) = &mut m.export.choose {
            c.format = postman_core::export::Format::Mbox;
            c.menu_at = Some(point(px(450.), px(140.)));
        }
        Ok(())
    }

    fn choose_ready(_: &Scene, m: &Mailer, _: &App) -> bool {
        m.export.choose.as_ref().is_some_and(|c| c.menu_at.is_some())
    }

    fn running(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let dir = "D:\\Mail\\Exchange".into();
        let run = super::Run { format: postman_core::export::Format::Mbox, dir, done: 3_120, total: 12_480, fetching: 140 };
        m.export.runs.insert(postman_core::export::ExportId(1), run);
        m.export.open = true;
        cx.notify();
        Ok(())
    }

    fn running_ready(_: &Scene, m: &Mailer, _: &App) -> bool {
        m.export.open && !m.export.runs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use postman_cache::{MailboxRow, Rights};
    use postman_core::export::Format;

    use super::{FORMATS, descendants, format_name, formats};

    fn mb(id: &str, parent: Option<&str>) -> MailboxRow {
        MailboxRow {
            id: id.into(),
            parent_id: parent.map(Into::into),
            name: id.into(),
            role: None,
            sort_order: 0,
            total: 0,
            unread: 0,
            rights: Rights::ALL,
        }
    }

    /// Подпапки для «С подпапками (N)» — все уровни, без самой папки.
    #[test]
    fn subfolders_all_levels() {
        let all = [mb("a", None), mb("b", Some("a")), mb("c", Some("b")), mb("d", None)];
        let mut got = descendants(&all, "a");
        got.sort();
        assert_eq!(got, ["b", "c"]);
        assert!(descendants(&all, "d").is_empty());
    }

    /// Форматы списка — только с писателем: PST не виден до S8-33б; подписи — у каждого.
    #[test]
    fn formats_listed() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        let listed: Vec<Format> = formats().map(|(f, ..)| f).collect();
        assert!(listed.contains(&Format::Eml) && listed.contains(&Format::Mbox) && listed.contains(&Format::Maildir));
        assert!(!listed.contains(&Format::Pst));
        assert_eq!(format_name(Format::Mbox), "mbox");
        for (_, _, hint) in FORMATS {
            assert!(!pets_i18n::tr!(hint).starts_with("export-"), "{hint}");
        }
    }
}
