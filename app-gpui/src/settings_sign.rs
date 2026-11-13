//! «Настройки → Подписи и шаблоны» (S5-09): несколько подписей на адрес отправителя, подпись по умолчанию для новых
//! писем и для ответов, шаблоны писем. Хранятся у учётной записи на этом компьютере (`postman_core::prefs::AccountPrefs`,
//! файл рядом с кэшем, запись в фоне); на сервере (FileNode, как категории) — [проверить]. Композер берёт их через
//! `AccountPrefs::signature` (S5-06).
//!
//! T2-52 (макет `SettingsSign`): у подписи — «Ж»/«К» (Markdown, как в письме) и «Вставить поле ▾» — `{имя}`,
//! `{должность}`, `{телефон}` из профиля учётной записи (свои поля раздела); у шаблона — команда `/…`, файлы, поля
//! `{…}` со «Спрашивать значения полей при вставке» (`postman_core::fill`). Справа — «Предпросмотр» нового письма и
//! ответа с подписью, как его увидит получатель.
//!
//! T3-07 (решение владельца 2026-11-11): «Картинка» — логотип в подпись (PNG, JPEG, GIF до 1 МБ, выбор файла
//! Windows): в тексте подписи — `![имя](cid:…)`, байты — файлом рядом с настройками (`SignImage`), в письме — вложение
//! inline. Предпросмотр показывает картинку (раскодирует воркер, как картинки писем).

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_compose::display::{Look as Marks, Span};
use postman_core::prefs::{AccountPrefs, ImageError, SignImage, Signature, Template, Writer};

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsView, group, link, now_ms, row, seg, seg_group};

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
    /// Шаблон: команда `/…`, файлы, спрашивать ли поля.
    command: Entity<InputState>,
    attachments: Vec<String>,
    ask: bool,
    /// Открыто меню «Вставить поле».
    field_menu: bool,
    /// Картинки подписи (T3-07): остаются те, на которые ссылается текст.
    images: Vec<SignImage>,
    /// Картинку не взяли (велика, не картинка, не прочиталась) — строкой под текстом.
    image_error: Option<String>,
}

/// Поля профиля для меню «Вставить поле»: ключ и что это.
const PROFILE_FIELDS: [(&str, &str); 3] = [
    ("settings-sign-field-name", "settings-sign-field-name-hint"),
    ("settings-sign-field-title", "settings-sign-field-title-hint"),
    ("settings-sign-field-phone", "settings-sign-field-phone-hint"),
];

/// Предпросмотр: новое письмо или ответ.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PreviewMode {
    #[default]
    New,
    Reply,
}

pub(crate) struct SignState {
    pub(crate) path: Option<PathBuf>,
    pub(crate) prefs: AccountPrefs,
    loaded: bool,
    pub(crate) edit: Option<SignEdit>,
    writer: Writer,
    /// Профиль для полей подписи: имя, должность, телефон (правка — сразу в настройки учётной записи).
    profile: [Entity<InputState>; 3],
    /// Поля профиля — заново из загруженных настроек (при отрисовке).
    profile_sync: bool,
    pub(crate) preview: PreviewMode,
    /// Кадры картинок подписей для предпросмотра по `cid` (воркер); `None` — не раскодировалась.
    pub(crate) images: HashMap<String, Option<Arc<RenderImage>>>,
    _subscriptions: Vec<Subscription>,
}

impl SignState {
    pub(crate) fn new(cache: Option<PathBuf>, window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let mut input = |key: &str, cx: &mut Context<SettingsView>| cx.new(|cx| InputState::new(window, cx).placeholder(tr!(key)));
        let profile = [
            input("settings-sign-profile-name-placeholder", cx),
            input("settings-sign-profile-title-placeholder", cx),
            input("settings-sign-profile-phone-placeholder", cx),
        ];
        let mut subs = Vec::new();
        for (i, p) in profile.iter().enumerate() {
            subs.push(cx.subscribe_in(p, window, move |this: &mut SettingsView, state, ev: &gpui_kit::component::input::InputEvent, _, cx| {
                if !matches!(ev, gpui_kit::component::input::InputEvent::Change) || this.sign.profile_sync {
                    return;
                }
                let v = state.read(cx).value().to_string();
                let p = &mut this.sign.prefs.profile;
                let slot = match i {
                    0 => &mut p.name,
                    1 => &mut p.title,
                    _ => &mut p.phone,
                };
                if *slot != v {
                    *slot = v;
                    this.sign_save(cx);
                    cx.notify();
                }
            }));
        }
        Self {
            path: cache,
            prefs: AccountPrefs::default(),
            loaded: false,
            edit: None,
            writer: Writer::default(),
            profile,
            profile_sync: true,
            preview: PreviewMode::New,
            images: HashMap::new(),
            _subscriptions: subs,
        }
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
                    tracing::error!("signature: signatures not read: {e}");
                    AccountPrefs::default()
                }
            };
            let _ = this.update(cx, |v, cx| {
                v.sign.prefs = prefs;
                v.sign.loaded = true;
                v.sign.profile_sync = true;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn sign_save(&mut self, cx: &mut Context<Self>) {
        self.sign.prefs = std::mem::take(&mut self.sign.prefs).normalized();
        cx.emit(crate::settings_tab::SettingsEvent::AccountPrefs(self.sign.prefs.clone()));
        let Some(file) = self.sign.file() else { return };
        let (prefs, writer) = (self.sign.prefs.clone(), self.sign.writer.clone());
        let seq = writer.next();
        cx.background_spawn(async move {
            if let Err(e) = writer.write(seq, || prefs.save(&file)) {
                tracing::error!("signature: signatures not written: {e}");
            }
        })
        .detach();
    }

    pub(crate) fn sign_edit(&mut self, id: String, template: bool, window: &mut Window, cx: &mut Context<Self>) {
        let p = &self.sign.prefs;
        let tpl = p.templates.iter().find(|t| t.id == id).cloned();
        let (name, subject, text, identity) = if template {
            let t = tpl.as_ref();
            (t.map(|t| t.name.clone()), t.map(|t| t.subject.clone()), t.map(|t| t.body.clone()), None)
        } else {
            let s = p.signatures.iter().find(|s| s.id == id);
            (s.map(|s| s.name.clone()), None, s.map(|s| s.text.clone()), s.and_then(|s| s.identity.clone()))
        };
        let images = if template { Vec::new() } else { p.signatures.iter().find(|s| s.id == id).map(|s| s.images.clone()).unwrap_or_default() };
        let (command, attachments, ask) = tpl.map_or((String::new(), Vec::new(), true), |t| (t.command, t.attachments, t.ask));
        let command = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-tpl-command-placeholder")).default_value(command));
        let name =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-sign-name-placeholder")).default_value(name.unwrap_or_default()));
        let subject =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-tpl-subject-placeholder")).default_value(subject.unwrap_or_default()));
        let text =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(tr!("settings-sign-text-placeholder")).default_value(text.unwrap_or_default()));
        name.update(cx, |s, cx| s.focus(window, cx));
        self.sign.edit =
            Some(SignEdit { id, template, name, subject, text, identity, command, attachments, ask, field_menu: false, images, image_error: None });
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
            let command = e.command.read(cx).value().trim().trim_start_matches('/').to_owned();
            let t = Template { id: e.id.clone(), name, subject, body: text, command, attachments: e.attachments.clone(), ask: e.ask };
            match p.templates.iter_mut().find(|x| x.id == e.id) {
                Some(x) => *x = t,
                None => p.templates.push(t),
            }
        } else {
            // Картинка, ссылку на которую стёрли из текста, уходит из подписи (и её файл — с диска).
            let images = e.images.iter().filter(|i| text.contains(&format!("cid:{}", i.cid))).cloned().collect();
            let s = Signature { id: e.id.clone(), name, identity: e.identity.clone(), text, images };
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

    /// «Ж» и «К»: Markdown вокруг выделения текста подписи (как в письме).
    fn sign_format(&mut self, mark: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = &self.sign.edit else { return };
        let text = e.text.clone();
        let (value, sel) = {
            let s = text.read(cx);
            (s.value().to_string(), s.selected_range())
        };
        let (start, end) = (sel.start.min(value.len()), sel.end.min(value.len()));
        if !value.is_char_boundary(start) || !value.is_char_boundary(end) {
            return;
        }
        let next = format!("{}{mark}{}{mark}{}", &value[..start], &value[start..end], &value[end..]);
        text.update(cx, |s, cx| {
            s.set_value(next, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// «Вставить поле»: `{поле}` у курсора текста подписи.
    fn sign_field(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = &mut self.sign.edit else { return };
        e.field_menu = false;
        let text = e.text.clone();
        let (value, at) = {
            let s = text.read(cx);
            (s.value().to_string(), s.selected_range().end)
        };
        let at = if value.is_char_boundary(at.min(value.len())) { at.min(value.len()) } else { value.len() };
        let token = format!("{{{}}}", tr!(key));
        let next = format!("{}{token}{}", &value[..at], &value[at..]);
        text.update(cx, |s, cx| {
            s.set_value(next, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// «Картинка» (T3-07): выбор файла Windows, чтение в фоне; PNG, JPEG или GIF до 1 МБ — в подпись, ссылкой
    /// `![имя](cid:…)` с новой строки у курсора текста.
    fn sign_pick_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let file = postman_core::files::safe_name(&name);
            let read = cx
                .background_spawn(async move {
                    // Размер — до чтения: большой файл в память не тянем.
                    let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                    if len > postman_core::prefs::MAX_SIGN_IMAGE as u64 {
                        return Ok(Err(ImageError::TooLarge));
                    }
                    std::fs::read(&path).map_err(|e| e.to_string()).map(|data| SignImage::new(&file, data, now_ms()))
                })
                .await;
            let name = path_name(&name);
            let _ = this.update_in(cx, |v, window, cx| {
                let Some(e) = &mut v.sign.edit else { return };
                match read {
                    Ok(Ok(img)) => {
                        e.image_error = None;
                        let token = img.markdown();
                        e.images.push(img);
                        v.sign_insert_line(token, window, cx);
                    }
                    Ok(Err(ImageError::TooLarge)) => e.image_error = Some(tr!("settings-sign-image-too-large", name = name)),
                    Ok(Err(ImageError::NotImage)) => e.image_error = Some(tr!("settings-sign-image-not-image", name = name)),
                    Err(error) => e.image_error = Some(tr!("settings-sign-image-failed", name = name, error = error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Строка в текст подписи у курсора (своей строкой): ссылка на картинку.
    fn sign_insert_line(&mut self, line: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = &self.sign.edit else { return };
        let text = e.text.clone();
        let (value, at) = {
            let s = text.read(cx);
            (s.value().to_string(), s.selected_range().end)
        };
        let at = if value.is_char_boundary(at.min(value.len())) { at.min(value.len()) } else { value.len() };
        let lead = if at == 0 || value[..at].ends_with('\n') { "" } else { "\n" };
        let trail = if value[at..].is_empty() || value[at..].starts_with('\n') { "" } else { "\n" };
        let next = format!("{}{lead}{line}{trail}{}", &value[..at], &value[at..]);
        text.update(cx, |s, cx| {
            s.set_value(next, window, cx);
            s.focus(window, cx);
        });
    }

    /// Картинки подписей для предпросмотра — в воркер (песочница), как картинки писем; раскодированные не повторяем.
    fn sign_decode(&mut self, images: Vec<SignImage>, cx: &mut Context<Self>) {
        let Some(worker) = self.info.worker.clone() else { return };
        for img in images {
            if self.sign.images.contains_key(&img.cid) {
                continue;
            }
            self.sign.images.insert(img.cid.clone(), None);
            if crate::reading::worker_missing() {
                continue;
            }
            let worker = worker.clone();
            let data = img.data.clone();
            let task = cx.background_spawn(async move {
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                w.decode_image(&data, PREVIEW_IMAGE_SIDE).and_then(crate::reading::render_image)
            });
            let cid = img.cid;
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |v, cx| match result {
                    Ok(image) => {
                        v.sign.images.insert(cid, Some(image));
                        cx.notify();
                    }
                    Err(e) => tracing::error!("signature: signature image not shown: {e}"),
                });
            })
            .detach();
        }
    }

    /// «Прикрепить файл» к шаблону: системный выбор файлов, пути — в шаблон.
    fn tpl_pick_files(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |v, cx| {
                    if let Some(e) = &mut v.sign.edit {
                        for p in paths {
                            let p = p.to_string_lossy().into_owned();
                            if !e.attachments.contains(&p) {
                                e.attachments.push(p);
                            }
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Ряд «Ж К · Вставить поле ▾» над текстом подписи.
    fn sign_toolbar(&self, e: &SignEdit, l: &Look, cx: &mut Context<Self>) -> Div {
        let tool = |id: &'static str, label: String| {
            div()
                .id(id)
                .children(crate::screens::spot(id))
                .h(px(26.))
                .min_w(px(26.))
                .px_1p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .child(label)
        };
        let menu = e.field_menu.then(|| {
            let items = PROFILE_FIELDS.iter().enumerate().map(|(i, (key, hint))| {
                let key = *key;
                div()
                    .id(("settings-sign-field", i))
                    .children(crate::screens::spot(("settings-sign-field", i)))
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded(l.px(Metric::RadiusControl))
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{{{}}}", tr!(key))))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!(hint)))
                    .on_click(cx.listener(move |this, _, window, cx| this.sign_field(key, window, cx)))
            });
            deferred(
                div()
                    .absolute()
                    .top(px(30.))
                    .left_0()
                    .min_w(px(260.))
                    .occlude()
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .rounded(l.px(Metric::RadiusOverlay))
                    .border_1()
                    .border_color(l.c(pets_ui::theme::Role::StrokeCard))
                    .bg(l.c(pets_ui::theme::Role::LayerOverlay))
                    .shadow_lg()
                    .children(items)
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        if let Some(e) = &mut this.sign.edit {
                            e.field_menu = false;
                        }
                        cx.notify();
                    })),
            )
            .with_priority(1)
        });
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                tool("settings-sign-bold", tr!("settings-sign-bold"))
                    .font_weight(FontWeight::BOLD)
                    .on_click(cx.listener(|this, _, window, cx| this.sign_format("**", window, cx))),
            )
            .child(
                tool("settings-sign-italic", tr!("settings-sign-italic"))
                    .italic()
                    .on_click(cx.listener(|this, _, window, cx| this.sign_format("*", window, cx))),
            )
            .child(div().w(px(1.)).h(px(16.)).mx_1().bg(l.border()))
            .child(
                div()
                    .relative()
                    .child(tool("settings-sign-field-menu", format!("{} ▾", tr!("settings-sign-insert-field"))).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(e) = &mut this.sign.edit {
                                e.field_menu = !e.field_menu;
                            }
                            cx.notify();
                        },
                    )))
                    .children(menu),
            )
            .child(div().w(px(1.)).h(px(16.)).mx_1().bg(l.border()))
            .child(
                tool("settings-sign-image", tr!("settings-sign-image"))
                    .gap_1()
                    .child(crate::chrome::icon(crate::chrome::glyph::PICTURE, 14.))
                    .on_click(cx.listener(|this, _, window, cx| this.sign_pick_image(window, cx))),
            )
    }

    /// Шаблон: команда, файлы, поля и «Спрашивать значения полей при вставке».
    fn tpl_extras(&self, e: &SignEdit, l: &Look, cx: &mut Context<Self>) -> Vec<Div> {
        let label = |t: String| div().w(px(110.)).flex_none().pt_1().text_color(l.text2()).child(t);
        let text = format!("{}\n{}", e.subject.read(cx).value(), e.text.read(cx).value());
        let own = postman_core::fill::own_fields(&text);
        let fields =
            if own.is_empty() { tr!("settings-tpl-fields-none") } else { own.iter().map(|f| format!("{{{f}}}")).collect::<Vec<_>>().join(", ") };
        let files = e.attachments.iter().enumerate().map(|(i, p)| {
            let name = std::path::Path::new(p).file_name().map_or_else(|| p.clone(), |n| n.to_string_lossy().into_owned());
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded(l.px(Metric::RadiusControl))
                .border_1()
                .border_color(l.border())
                .bg(l.panel())
                .child(crate::chrome::icon(crate::chrome::glyph::ATTACH, 12.).text_color(l.text2()))
                .child(name)
                .child(link(("settings-tpl-file-remove", i), "×".into(), l).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(e) = &mut this.sign.edit
                        && i < e.attachments.len()
                    {
                        e.attachments.remove(i);
                    }
                    cx.notify();
                })))
        });
        let ask = e.ask;
        vec![
            div().flex().gap_4().child(label(tr!("settings-tpl-command"))).child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_color(l.text2()).child("/"))
                    .child(div().w(px(200.)).relative().children(crate::screens::spot("settings-tpl-command")).child(Input::new(&e.command))),
            ),
            div().flex().gap_4().child(label(tr!("settings-tpl-files"))).child(
                div().flex_1().flex().flex_wrap().items_center().gap_2().children(files).child(
                    link("settings-tpl-attach", format!("+ {}", tr!("settings-tpl-attach")), l)
                        .on_click(cx.listener(|this, _, _, cx| this.tpl_pick_files(cx))),
                ),
            ),
            div().flex().gap_4().child(label(tr!("settings-tpl-fields"))).child(
                div().flex_1().flex().flex_col().gap_2().child(div().text_color(l.text2()).child(fields)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(crate::settings_tab::toggle("settings-tpl-ask", ask, l).on_click(cx.listener(|this, _, _, cx| {
                            if let Some(e) = &mut this.sign.edit {
                                e.ask = !e.ask;
                            }
                            cx.notify();
                        })))
                        .child(div().flex().flex_col().child(tr!("settings-tpl-ask")).child(
                            div().text_size(px(12.)).text_color(l.text2()).child(if ask {
                                tr!("settings-tpl-ask-on")
                            } else {
                                tr!("settings-tpl-ask-off")
                            }),
                        )),
                ),
            ),
        ]
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
            .when(!e.template, |d| d.child(div().pl(px(126.)).child(self.sign_toolbar(e, l, cx))))
            .child(
                div().flex().gap_4().child(label(if e.template { tr!("settings-tpl-body") } else { tr!("settings-sign-text") })).child(
                    div()
                        .flex_1()
                        .h(px(120.))
                        .relative()
                        .children(crate::screens::spot("settings-sign-text"))
                        .child(Textarea::new(&e.text).h(relative(1.))),
                ),
            )
            .when(!e.template, |d| d.child(div().pl(px(126.)).text_size(px(12.)).text_color(l.text2()).child(tr!("settings-sign-fields"))))
            .when(!e.template, |d| d.child(div().pl(px(126.)).text_size(px(12.)).text_color(l.text2()).child(tr!("settings-sign-image-hint"))))
            .children(e.image_error.clone().map(|err| {
                div()
                    .id("settings-sign-image-error")
                    .children(crate::screens::spot("settings-sign-image-error"))
                    .pl(px(126.))
                    .text_size(px(12.))
                    .text_color(l.error())
                    .child(err)
            }))
            .when(e.template, |d| d.children(self.tpl_extras(e, l, cx)))
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

    /// Предпросмотр: правимая подпись или подпись по умолчанию первого адреса — с полями профиля, в новом письме или
    /// над цитатой ответа.
    fn sign_preview(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = &self.sign.prefs;
        let reply = self.sign.preview == PreviewMode::Reply;
        let me = self.info.identities.first().cloned().unwrap_or_default();
        let editing = self
            .sign
            .edit
            .as_ref()
            .filter(|e| !e.template)
            .map(|e| (e.name.read(cx).value().to_string(), e.text.read(cx).value().to_string(), e.identity.clone()));
        let (name, text, identity) = editing
            .or_else(|| p.signature(&me, reply).or(p.signatures.first()).map(|s| (s.name.clone(), s.text.clone(), s.identity.clone())))
            .unwrap_or_default();
        let images = &self.sign.images;
        let mut profile = p.profile.clone();
        if profile.name.trim().is_empty() {
            profile.name = tr!("settings-sign-preview-me");
        }
        let signature = postman_core::fill::signature(text.trim_end(), &profile);
        let marks = postman_compose::display::display(&signature);
        let modes: Vec<Stateful<Div>> = [(PreviewMode::New, "settings-sign-preview-new"), (PreviewMode::Reply, "settings-sign-preview-reply")]
            .into_iter()
            .enumerate()
            .map(|(i, (m, key))| {
                seg(("settings-sign-preview", i), tr!(key), self.sign.preview == m, true, l).on_click(cx.listener(move |this, _, _, cx| {
                    this.sign.preview = m;
                    cx.notify();
                }))
            })
            .collect();
        let line = |k: String, v: String| div().flex().gap_2().text_size(px(12.)).child(div().w(px(40.)).text_color(l.text2()).child(k)).child(v);
        let letter = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(l.px(Metric::RadiusControl))
            .border_1()
            .border_color(l.border())
            // Светлая тема — так письмо увидит получатель: подложка письма — как у области чтения.
            .bg(l.panel())
            .child(line(tr!("settings-sign-preview-to"), tr!("settings-sign-preview-to-name")))
            .child(line(
                tr!("settings-tpl-subject"),
                if reply { tr!("settings-sign-preview-subject-re") } else { tr!("settings-sign-preview-subject") },
            ))
            .child(div().h(px(1.)).bg(l.border()))
            .child(div().text_size(px(12.)).child(if reply { tr!("settings-sign-preview-body-re") } else { tr!("settings-sign-preview-body") }))
            .child(
                div()
                    .id("settings-sign-preview-text")
                    .children(crate::screens::spot("settings-sign-preview-text"))
                    .flex()
                    .flex_col()
                    .text_size(px(12.))
                    .text_color(l.text2())
                    .children(line_ranges(&signature).map(|r| preview_line(&signature, r, &marks, images)).collect::<Vec<_>>()),
            )
            .when(reply, |d| {
                d.child(
                    div()
                        .pl_2()
                        .border_l_2()
                        .border_color(l.border())
                        .text_size(px(12.))
                        .text_color(l.text3())
                        .child(tr!("settings-sign-preview-quote")),
                )
            });
        let title =
            format!("{} · {}", if name.is_empty() { tr!("settings-sign-none") } else { name }, identity.unwrap_or_else(|| tr!("settings-sign-any")));
        let note = if reply { tr!("settings-sign-preview-note-re") } else { tr!("settings-sign-preview-note") };
        crate::settings_tab::preview_card(div().flex().flex_col().gap_2().child(seg_group(modes, l)).child(letter), title, note, l)
    }

    /// Профиль для полей подписи: имя, должность, телефон.
    fn sign_profile(&self, l: &Look) -> Div {
        let field = |key: String, input: &Entity<InputState>, id: &'static str| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().w(px(150.)).flex_none().text_color(l.text2()).child(key))
                .child(div().w(px(320.)).relative().children(crate::screens::spot(id)).child(Input::new(input)))
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(group(tr!("settings-sign-profile"), l))
            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-sign-profile-hint")))
            .child(field(format!("{{{}}}", tr!("settings-sign-field-name")), &self.sign.profile[0], "settings-sign-profile-name"))
            .child(field(format!("{{{}}}", tr!("settings-sign-field-title")), &self.sign.profile[1], "settings-sign-profile-title"))
            .child(field(format!("{{{}}}", tr!("settings-sign-field-phone")), &self.sign.profile[2], "settings-sign-profile-phone"))
    }

    /// «По умолчанию» (T2-58): какую подпись ставить — первая подпись адреса в новые письма и в ответы.
    fn sign_reset(&mut self, cx: &mut Context<Self>) {
        let p = &mut self.sign.prefs;
        p.defaults.clear();
        for addr in &self.info.identities {
            let first = p.signatures_for(addr).next().map(|s| s.id.clone());
            p.defaults.insert(addr.clone(), postman_core::prefs::Defaults { new: first.clone(), reply: first });
        }
        self.sign_save(cx);
        crate::prefs::update(cx, |p| p.replies = Default::default());
        cx.notify();
    }

    /// «Ответы и пересылка» (#655, как «Почта → Ответы и пересылка» Outlook): цитировать ли исходное письмо в
    /// «Ответить» и «Ответить всем» и сколько ждать отмены отправки (CMF-03). Общая настройка пользователя
    /// (`Prefs::replies`), не учётной записи.
    fn replies(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        use postman_core::prefs::ReplyQuote;
        let current = crate::prefs::with(cx, |p| p.replies.quote);
        let items: Vec<Stateful<Div>> = ReplyQuote::ALL
            .into_iter()
            .enumerate()
            .map(|(i, q)| {
                let name = tr!(match q {
                    ReplyQuote::Include => "settings-reply-quote-include",
                    ReplyQuote::Omit => "settings-reply-quote-omit",
                });
                seg(("settings-reply-quote", i), name, current == q, true, l).on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.replies.quote = q);
                    cx.notify();
                }))
            })
            .collect();
        // «Отмена отправки» (CMF-03): сколько письмо ждёт после «Отправить» — 0 / 5 / 10 / 30 с.
        let undo = crate::prefs::with(cx, |p| p.replies.undo());
        let delays: Vec<Stateful<Div>> = postman_core::send_later::UNDO_DELAYS
            .into_iter()
            .map(|secs| {
                let name = if secs == 0 { tr!("settings-undo-send-off") } else { tr!("settings-undo-send-secs", s = secs) };
                seg(("settings-undo-send", usize::from(secs)), name, undo == secs, true, l).on_click(cx.listener(move |_, _, _, cx| {
                    crate::prefs::update(cx, |p| p.replies.undo_secs = secs);
                    cx.notify();
                }))
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .child(group(tr!("settings-replies-group"), l))
            .child(row(tr!("settings-reply-quote"), Some(tr!("settings-reply-quote-hint")), seg_group(items, l), l))
            .child(row(tr!("settings-undo-send"), Some(tr!("settings-undo-send-hint")), seg_group(delays, l), l))
    }

    pub(crate) fn signatures(&mut self, l: &Look, window: &mut Window, cx: &mut Context<Self>) -> (Option<Stateful<Div>>, AnyElement, Vec<Div>) {
        if std::mem::take(&mut self.sign.profile_sync) {
            let pr = self.sign.prefs.profile.clone();
            self.sign.profile_sync = true;
            for (input, v) in self.sign.profile.clone().iter().zip([pr.name, pr.title, pr.phone]) {
                input.update(cx, |s, cx| s.set_value(v, window, cx));
            }
            self.sign.profile_sync = false;
        }
        // Картинки предпросмотра (T3-07): правимой подписи и сохранённых — в воркер, если ещё не раскодированы.
        let mut shown: Vec<SignImage> = self.sign.edit.as_ref().map(|e| e.images.clone()).unwrap_or_default();
        shown.extend(self.sign.prefs.sign_images().cloned());
        self.sign_decode(shown, cx);
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
            .child(self.replies(l, cx))
            .child(tpl_head)
            .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-tpl-hint")))
            .child(tpls)
            .child(self.sign_profile(l))
            .child(div().pt_3().text_size(px(12.)).text_color(l.text2()).child(if self.sign.path.is_some() {
                tr!("settings-sign-local")
            } else {
                tr!("settings-sign-demo")
            }));
        let reset = crate::buttons::button("settings-sign-reset", tr!("settings-reset"), false, l)
            .on_click(cx.listener(|this, _, _, cx| this.sign_reset(cx)));
        let aside = vec![
            self.sign_preview(l, cx),
            crate::settings_tab::aside_card(tr!("settings-reading-aside"), vec![tr!("settings-sign-aside-1"), tr!("settings-sign-aside-2")], l),
        ];
        (Some(reset), body.into_any_element(), aside)
    }
}

/// Образец для снимка `settings-sign` (демо): две подписи — основная для новых писем и короткая для ответов, шаблон.
pub(crate) fn screen_sample(identities: &[String]) -> AccountPrefs {
    let mut p = AccountPrefs::default();
    let text = "С уважением,\n**{имя}**\n{должность}".to_owned(); // i18n-ok: образец подписи
    p.signatures.push(Signature { id: "s1".into(), name: "Основная".into(), identity: None, text, images: Vec::new() }); // i18n-ok: образец
    p.signatures
        .push(Signature { id: "s2".into(), name: "Короткая".into(), identity: None, text: "— Иван".into(), images: Vec::new() }); // i18n-ok: образец
    p.templates.push(Template {
        id: "t1".into(),
        name: "Коммерческое предложение".into(),                   // i18n-ok: образец шаблона
        subject: "Коммерческое предложение для {Компания}".into(), // i18n-ok: образец
        body: "Добрый день! Направляю предложение для {Компания} по итогам {Дата}.".into(), // i18n-ok: образец
        command: "кп".into(),                                      // i18n-ok: образец
        attachments: Vec::new(),
        ask: true,
    });
    let name = "Иван Петров".to_owned(); // i18n-ok: образец
    let title = "Руководитель отдела продаж".to_owned(); // i18n-ok: образец
    p.profile = postman_core::prefs::Profile { name, title, phone: String::new() };
    if let Some(me) = identities.first() {
        p.defaults.insert(me.clone(), postman_core::prefs::Defaults { new: Some("s1".into()), reply: Some("s2".into()) });
    }
    p
}

/// Образец для снимков T3-07: основная подпись — с логотипом (`![…](cid:…)` под должностью), по умолчанию для новых
/// писем всех адресов `identities`.
pub(crate) fn screen_sample_logo(identities: &[String]) -> AccountPrefs {
    let mut p = screen_sample(identities);
    let Ok(img) = SignImage::new("logo.png", sample_logo(), 1_000) else { return p };
    if let Some(s) = p.signatures.iter_mut().find(|s| s.id == "s1") {
        s.text = format!("{}\n{}", s.text, img.markdown());
        s.images.push(img);
    }
    for me in identities {
        p.defaults.insert(me.clone(), postman_core::prefs::Defaults { new: Some("s1".into()), reply: Some("s2".into()) });
    }
    p
}

/// Логотип образца: 192×48, синяя плашка с тремя белыми полосами (PNG собирается здесь — файлов в репозитории нет).
pub(crate) fn sample_logo() -> Vec<u8> {
    let (w, h) = (192u32, 48u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let edge = x < 4 || y < 4 || x >= w - 4 || y >= h - 4;
            let stripe = (12..36).contains(&y) && [(16, 40), (48, 72), (80, 104)].iter().any(|&(a, b)| (a..b).contains(&x));
            let bar = (20..28).contains(&y) && (116..176).contains(&x);
            let rgba: [u8; 4] = if edge {
                [0, 0, 0, 0]
            } else if stripe || bar {
                [255, 255, 255, 255]
            } else {
                [0, 103, 192, 255]
            };
            px.extend_from_slice(&rgba);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let written = enc.write_header().and_then(|mut wr| wr.write_image_data(&px));
        if let Err(e) = written {
            tracing::warn!("signature: sample logo not created: {e}");
        }
    }
    out
}

/// Сторона кадра картинки подписи из воркера и высота её в предпросмотре.
const PREVIEW_IMAGE_SIDE: u32 = 400;
const PREVIEW_IMAGE_H: f32 = 48.0;

/// Имя файла для строки ошибки.
fn path_name(name: &str) -> String {
    if name.is_empty() { "?".into() } else { name.to_owned() }
}

/// Строки подписи — байтовые отрезки без перевода строки.
fn line_ranges(s: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut at = 0;
    s.split_inclusive('\n').map(move |line| {
        let r = at..at + line.trim_end_matches(['\n', '\r']).len();
        at += line.len();
        r
    })
}

/// Строка `r` предпросмотра подписи: картинки `![…](cid:…)` — кадром (не раскодирована — её подписью), остальное —
/// разметкой письма.
fn preview_line(sig: &str, r: Range<usize>, marks: &[Span], images: &HashMap<String, Option<Arc<RenderImage>>>) -> Div {
    let line = &sig[r.clone()];
    let Some(at) = line.find("![") else { return md_line(sig, r, marks) };
    let rest = &line[at..];
    let parsed = rest.find("](cid:").and_then(|m| {
        let alt = &rest[2..m];
        let tail = &rest[m + "](cid:".len()..];
        let close = tail.find(')')?;
        Some((alt, &tail[..close], r.start + at + m + "](cid:".len() + close + 1))
    });
    let Some((alt, cid, after)) = parsed else { return md_line(sig, r, marks) };
    let picture = match images.get(cid).cloned().flatten() {
        Some(image) => {
            let size = image.size(0);
            let (w, h) = (size.width.0 as f32, size.height.0 as f32);
            let scale = (PREVIEW_IMAGE_H / h.max(1.)).min(1.);
            div().py_1().child(img(image).w(px(w * scale)).h(px(h * scale)))
        }
        None => div().italic().child(format!("[{alt}]")),
    };
    div().flex().flex_wrap().items_center().gap_1().child(md_line(sig, r.start..r.start + at, marks)).child(picture).child(preview_line(
        sig,
        after..r.end,
        marks,
        images,
    ))
}

/// Строка подписи с разметкой письма (#654): признаки — те же, что у поля письма и при отправке
/// (`postman_compose::display`), знаки разметки скрыты.
fn md_line(sig: &str, r: Range<usize>, marks: &[Span]) -> Div {
    let run = |text: &str, m: Option<Marks>| {
        let m = m.unwrap_or_default();
        div()
            .whitespace_nowrap()
            .when(m.strong, |d| d.font_weight(FontWeight::BOLD))
            .when(m.emphasis, |d| d.italic())
            .when(m.strike, |d| d.line_through())
            .when(m.underline, |d| d.underline())
            .child(text.to_owned())
    };
    let mut d = div().flex().flex_wrap().min_h(px(16.));
    let mut at = r.start;
    for s in marks.iter().filter(|s| s.range.end > r.start && s.range.start < r.end) {
        let (a, b) = (s.range.start.max(r.start), s.range.end.min(r.end));
        if a > at {
            d = d.child(run(&sig[at..a], None));
        }
        if !s.look.markup {
            d = d.child(run(&sig[a..b], Some(s.look)));
        }
        at = b;
    }
    if at < r.end {
        d = d.child(run(&sig[at..r.end], None));
    }
    d
}

fn badge(text: String, l: &Look) -> Div {
    div().px_1p5().rounded_sm().bg(l.fill()).text_size(px(11.)).text_color(l.text2()).child(text)
}
