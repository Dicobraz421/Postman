//! «Настройки → Подписи и шаблоны» (S5-09): несколько подписей на адрес отправителя, подпись по умолчанию для новых
//! писем и для ответов, шаблоны писем. Хранятся у учётной записи на этом компьютере (`postman_core::prefs::AccountPrefs`,
//! файл рядом с кэшем, запись в фоне); на сервере (FileNode, как категории) — [проверить]. Композер берёт их через
//! `AccountPrefs::signature` (S5-06).

use std::path::PathBuf;

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_core::prefs::{AccountPrefs, Signature, Template, Writer};

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsView, group, link, now_ms, seg, seg_group};

/// Правка подписи или шаблона на месте.
pub(crate) struct SignEdit {
    /// Id правимой подписи или шаблона.
    id: String,
    template: bool,
    name: Entity<InputState>,
    /// Тема шаблона.
    subject: Entity<InputState>,
    text: Entity<TextareaState>,
    /// Адрес подписи; `None` — любой.
    identity: Option<String>,
}

pub(crate) struct SignState {
    pub(crate) path: Option<PathBuf>,
    pub(crate) prefs: AccountPrefs,
    loaded: bool,
    edit: Option<SignEdit>,
    writer: Writer,
}

impl SignState {
    pub(crate) fn new(cache: Option<PathBuf>, _window: &mut Window, _cx: &mut Context<SettingsView>) -> Self {
        Self { path: cache, prefs: AccountPrefs::default(), loaded: false, edit: None, writer: Writer::default() }
    }

    fn file(&self) -> Option<PathBuf> {
        self.path.as_deref().map(AccountPrefs::beside)
    }
}

impl SettingsView {
    /// Подписи и шаблоны учётной записи — из файла рядом с её кэшем (в фоне). Демо — только в памяти.
    pub(crate) fn sign_load(&mut self, cx: &mut Context<Self>) {
        self.sign.loaded = false;
        self.sign.edit = None;
        let Some(file) = self.sign.file() else {
            self.sign.prefs = AccountPrefs::default();
            self.sign.loaded = true;
            return;
        };
        let task = cx.background_spawn(async move { AccountPrefs::load(&file) });
        cx.spawn(async move |this, cx| {
            let prefs = match task.await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("{e}");
                    AccountPrefs::default()
                }
            };
            let _ = this.update(cx, |v, cx| {
                v.sign.prefs = prefs;
                v.sign.loaded = true;
                cx.notify();
            });
        })
        .detach();
    }

    fn sign_save(&mut self, cx: &mut Context<Self>) {
        self.sign.prefs = std::mem::take(&mut self.sign.prefs).normalized();
        cx.emit(crate::settings_tab::SettingsEvent::AccountPrefs(self.sign.prefs.clone()));
        let Some(file) = self.sign.file() else { return };
        let (prefs, writer) = (self.sign.prefs.clone(), self.sign.writer.clone());
        let seq = writer.next();
        cx.background_spawn(async move {
            if let Err(e) = writer.write(seq, || prefs.save(&file)) {
                tracing::warn!("{e}");
            }
        })
        .detach();
    }

    fn sign_edit(&mut self, id: String, template: bool, window: &mut Window, cx: &mut Context<Self>) {
        let p = &self.sign.prefs;
        let (name, subject, text, identity) = if template {
            let t = p.templates.iter().find(|t| t.id == id);
            (t.map(|t| t.name.clone()), t.map(|t| t.subject.clone()), t.map(|t| t.body.clone()), None)
        } else {
            let s = p.signatures.iter().find(|s| s.id == id);
            (s.map(|s| s.name.clone()), None, s.map(|s| s.text.clone()), s.and_then(|s| s.identity.clone()))
        };
        let name =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-sign-name-placeholder")).default_value(name.unwrap_or_default()));
        let subject =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-tpl-subject-placeholder")).default_value(subject.unwrap_or_default()));
        let text =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(tr!("settings-sign-text-placeholder")).default_value(text.unwrap_or_default()));
        name.update(cx, |s, cx| s.focus(window, cx));
        self.sign.edit = Some(SignEdit { id, template, name, subject, text, identity });
        cx.notify();
    }

    fn sign_add(&mut self, template: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.sign.prefs.new_id(if template { 't' } else { 's' }, now_ms());
        self.sign_edit(id, template, window, cx);
    }

    fn sign_commit(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.sign.edit.take() else { return };
        let name = e.name.read(cx).value().trim().to_owned();
        let name = if name.is_empty() { tr!("settings-sign-untitled") } else { name };
        let text = e.text.read(cx).value().to_string();
        let p = &mut self.sign.prefs;
        if e.template {
            let subject = e.subject.read(cx).value().to_string();
            let t = Template { id: e.id.clone(), name, subject, body: text };
            match p.templates.iter_mut().find(|x| x.id == e.id) {
                Some(x) => *x = t,
                None => p.templates.push(t),
            }
        } else {
            let s = Signature { id: e.id.clone(), name, identity: e.identity.clone(), text };
            match p.signatures.iter_mut().find(|x| x.id == e.id) {
                Some(x) => *x = s,
                None => p.signatures.push(s),
            }
        }
        self.sign_save(cx);
        cx.notify();
    }

    fn sign_delete(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.sign.edit.take() else { return };
        if e.template {
            self.sign.prefs.templates.retain(|t| t.id != e.id);
        } else {
            self.sign.prefs.remove_signature(&e.id);
        }
        self.sign_save(cx);
        cx.notify();
    }

    fn set_default(&mut self, identity: String, reply: bool, id: Option<String>, cx: &mut Context<Self>) {
        let d = self.sign.prefs.defaults.entry(identity).or_default();
        if reply {
            d.reply = id;
        } else {
            d.new = id;
        }
        self.sign_save(cx);
        cx.notify();
    }

    fn sign_editor(&self, e: &SignEdit, l: &Look, cx: &mut Context<Self>) -> Div {
        let label = |t: String| div().w(px(110.)).flex_none().pt_1().text_color(l.text2()).child(t);
        let mut ids =
            vec![seg("settings-sign-any", tr!("settings-sign-any"), e.identity.is_none(), true, l).on_click(cx.listener(|this, _, _, cx| {
                if let Some(e) = &mut this.sign.edit {
                    e.identity = None;
                }
                cx.notify();
            }))];
        for (i, addr) in self.info.identities.iter().enumerate() {
            let on = e.identity.as_deref() == Some(addr.as_str());
            let a = addr.clone();
            ids.push(seg(("settings-sign-identity", i), addr.clone(), on, true, l).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(e) = &mut this.sign.edit {
                    e.identity = Some(a.clone());
                }
                cx.notify();
            })));
        }
        let exists =
            if e.template { self.sign.prefs.templates.iter().any(|t| t.id == e.id) } else { self.sign.prefs.signatures.iter().any(|s| s.id == e.id) };
        div()
            .my_2()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded(l.px(Metric::RadiusOverlay))
            .bg(l.fill_passive())
            .border_1()
            .border_color(l.border())
            .child(div().flex().gap_4().child(label(tr!("settings-sign-name"))).child(div().w(px(320.)).child(Input::new(&e.name))))
            .when(!e.template, |d| d.child(div().flex().gap_4().child(label(tr!("settings-sign-identity"))).child(seg_group(ids, l))))
            .when(e.template, |d| {
                d.child(div().flex().gap_4().child(label(tr!("settings-tpl-subject"))).child(div().flex_1().child(Input::new(&e.subject))))
            })
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(label(if e.template { tr!("settings-tpl-body") } else { tr!("settings-sign-text") }))
                    .child(div().flex_1().h(px(120.)).child(Textarea::new(&e.text).h(relative(1.)))),
            )
            .when(!e.template, |d| d.child(div().pl(px(126.)).text_size(px(12.)).text_color(l.text2()).child(tr!("settings-sign-fields"))))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button("settings-sign-save", tr!("settings-save"), true, l).on_click(cx.listener(|this, _, _, cx| this.sign_commit(cx))))
                    .child(button("settings-sign-cancel", tr!("settings-cancel"), false, l).on_click(cx.listener(|this, _, _, cx| {
                        this.sign.edit = None;
                        cx.notify();
                    })))
                    .when(exists, |d| {
                        d.child(div().flex_1()).child(
                            link("settings-sign-delete", tr!("settings-delete"), l).on_click(cx.listener(|this, _, _, cx| this.sign_delete(cx))),
                        )
                    }),
            )
    }

    pub(crate) fn signatures(&mut self, l: &Look, _window: &mut Window, cx: &mut Context<Self>) -> (Option<Stateful<Div>>, AnyElement, Vec<Div>) {
        let p = self.sign.prefs.clone();
        let editing = self.sign.edit.as_ref().map(|e| (e.id.clone(), e.template));
        // Подписи.
        let mut sigs = div().flex().flex_col();
        for (i, s) in p.signatures.iter().enumerate() {
            let used_new = p.defaults.values().any(|d| d.new.as_deref() == Some(s.id.as_str()));
            let used_reply = p.defaults.values().any(|d| d.reply.as_deref() == Some(s.id.as_str()));
            let id = s.id.clone();
            sigs = sigs.child(
                div()
                    .id(("settings-sign-row", i))
                    .min_h(px(40.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(l.border())
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(s.name.clone()))
                    .when(used_new, |d| d.child(badge(tr!("settings-sign-for-new"), l)))
                    .when(used_reply, |d| d.child(badge(tr!("settings-sign-for-reply"), l)))
                    .child(div().flex_1())
                    .child(div().text_color(l.text2()).child(s.identity.clone().unwrap_or_else(|| tr!("settings-sign-any"))))
                    .on_click(cx.listener(move |this, _, window, cx| this.sign_edit(id.clone(), false, window, cx))),
            );
            if editing.as_ref().is_some_and(|(e, t)| *e == s.id && !t)
                && let Some(e) = &self.sign.edit
            {
                sigs = sigs.child(self.sign_editor(e, l, cx));
            }
        }
        if p.signatures.is_empty() && self.sign.loaded {
            sigs = sigs.child(div().py_2().text_color(l.text2()).child(tr!("settings-sign-empty")));
        }
        if let (Some((id, false)), Some(e)) = (&editing, &self.sign.edit)
            && !p.signatures.iter().any(|s| &s.id == id)
        {
            sigs = sigs.child(self.sign_editor(e, l, cx));
        }
        // Какую подпись ставить.
        let mut defaults = div().flex().flex_col();
        for (ai, addr) in self.info.identities.iter().enumerate() {
            let d = p.defaults.get(addr).cloned().unwrap_or_default();
            let options: Vec<(Option<String>, String)> = std::iter::once((None, tr!("settings-sign-none")))
                .chain(p.signatures_for(addr).map(|s| (Some(s.id.clone()), s.name.clone())))
                .collect();
            let line = |reply: bool| {
                let current = if reply { d.reply.clone() } else { d.new.clone() };
                let items: Vec<Stateful<Div>> = options
                    .iter()
                    .enumerate()
                    .map(|(oi, (id, name))| {
                        let (id, a) = (id.clone(), addr.clone());
                        let on = current == id;
                        seg(("settings-sign-default", (ai * 2 + usize::from(reply)) * 64 + oi), name.clone(), on, true, l)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_default(a.clone(), reply, id.clone(), cx)))
                    })
                    .collect();
                seg_group(items, l)
            };
            let new_line = line(false);
            let reply_line = line(true);
            defaults = defaults.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_3()
                    .border_b_1()
                    .border_color(l.border())
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(addr.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().w(px(150.)).text_color(l.text2()).child(tr!("settings-sign-new")))
                            .child(new_line),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().w(px(150.)).text_color(l.text2()).child(tr!("settings-sign-reply")))
                            .child(reply_line),
                    ),
            );
        }
        if self.info.identities.is_empty() {
            defaults = defaults.child(div().py_2().text_color(l.text2()).child(tr!("settings-sign-no-identities")));
        }
        // Шаблоны.
        let mut tpls = div().flex().flex_col();
        for (i, t) in p.templates.iter().enumerate() {
            let id = t.id.clone();
            let preview: String = t.body.lines().next().unwrap_or_default().chars().take(80).collect();
            tpls = tpls.child(
                div()
                    .id(("settings-tpl-row", i))
                    .min_h(px(40.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(l.border())
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(div().w(px(180.)).flex_none().truncate().font_weight(FontWeight::SEMIBOLD).child(t.name.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).child(preview))
                    .on_click(cx.listener(move |this, _, window, cx| this.sign_edit(id.clone(), true, window, cx))),
            );
            if editing.as_ref().is_some_and(|(e, tp)| *e == t.id && *tp)
                && let Some(e) = &self.sign.edit
            {
                tpls = tpls.child(self.sign_editor(e, l, cx));
            }
        }
        if p.templates.is_empty() && self.sign.loaded {
            tpls = tpls.child(div().py_2().text_color(l.text2()).child(tr!("settings-tpl-empty")));
        }
        if let (Some((id, true)), Some(e)) = (&editing, &self.sign.edit)
            && !p.templates.iter().any(|t| &t.id == id)
        {
            tpls = tpls.child(self.sign_editor(e, l, cx));
        }
        let head = |title: String, id: &'static str, label: String, template: bool, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_end()
                .gap_3()
                .child(div().flex_1().child(group(title, l)))
                .child(link(id, format!("+ {label}"), l).on_click(cx.listener(move |this, _, window, cx| this.sign_add(template, window, cx))))
        };
        let sig_head = head(tr!("settings-sign-group"), "settings-sign-add", tr!("settings-sign-add"), false, cx);
        let tpl_head = head(tr!("settings-tpl-group"), "settings-tpl-add", tr!("settings-tpl-add"), true, cx);
        let body = div()
            .flex()
            .flex_col()
            .child(sig_head)
            .child(sigs)
            .child(group(tr!("settings-sign-defaults"), l))
            .child(defaults)
            .child(tpl_head)
            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-tpl-hint")))
            .child(tpls)
            .child(div().pt_3().text_size(px(12.)).text_color(l.text2()).child(if self.sign.path.is_some() {
                tr!("settings-sign-local")
            } else {
                tr!("settings-sign-demo")
            }));
        (None, body.into_any_element(), Vec::new())
    }
}

/// Образец для снимка `settings-sign` (демо): две подписи — основная для новых писем и короткая для ответов, шаблон.
pub(crate) fn screen_sample(identities: &[String]) -> AccountPrefs {
    let mut p = AccountPrefs::default();
    let text = "С уважением,\nИван Петров\nотдел продаж".to_owned(); // i18n-ok: образец подписи
    p.signatures.push(Signature { id: "s1".into(), name: "Основная".into(), identity: None, text }); // i18n-ok: образец
    p.signatures.push(Signature { id: "s2".into(), name: "Короткая".into(), identity: None, text: "— Иван".into() }); // i18n-ok: образец
    p.templates.push(Template {
        id: "t1".into(),
        name: "Коммерческое предложение".into(),                       // i18n-ok: образец шаблона
        subject: "Предложение".into(),                                 // i18n-ok: образец
        body: "Добрый день! Направляю предложение по проекту.".into(), // i18n-ok: образец
    });
    if let Some(me) = identities.first() {
        p.defaults.insert(me.clone(), postman_core::prefs::Defaults { new: Some("s1".into()), reply: Some("s2".into()) });
    }
    p
}

fn badge(text: String, l: &Look) -> Div {
    div().px_1p5().rounded_sm().bg(l.fill()).text_size(px(11.)).text_color(l.text2()).child(text)
}
