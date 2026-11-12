//! Свои сертификаты S/MIME из файлов PKCS#12 (Linux, LIN-05b; решение владельца 2026-11-12 — ключи сотрудников в
//! файлах). Пути — в настройках (`security.smime_files`), пароль — в хранилище паролей системы
//! (`sys::credentials`, запись `Postman:smime:<путь>`); хранилища нет — пароль живёт до выхода и спрашивается снова
//! (ADR 0011 п. 4: секреты не на диск). Ключи открывает ядро (`postman_core::smime::load_personal_files`) — при
//! запуске и после каждого изменения; «Настройки → Безопасность → Свои сертификаты» — добавить, ввести пароль,
//! убрать (подтверждение строкой).

use std::collections::HashMap;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_core::smime::PersonalFileError;
use postman_smime::p12::P12Error;
use zeroize::Zeroizing;

use crate::buttons::button;
use crate::roles::Look;
use crate::settings_tab::{SettingsView, row};

/// Запись пароля файла в хранилище паролей.
fn credential_target(path: &str) -> String {
    format!("Postman:smime:{path}")
}

/// Что с файлом сейчас.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileStatus {
    /// Открыт: столько своих сертификатов с ключом.
    Open(usize),
    /// Пароля нет (не сохранён или хранилища нет) — нужен ввод.
    NeedPassword,
    /// Не открыт: ключ Fluent причины.
    Failed(&'static str),
}

/// Ключ Fluent причины, по которой файл не открыт (одна причина — один ключ, ADR 0011 п. 1).
pub(crate) fn reason(e: &PersonalFileError) -> &'static str {
    match e {
        PersonalFileError::Read(_) => "settings-certs-file-unreadable",
        PersonalFileError::Pkcs12(P12Error::BadPassword) => "settings-certs-file-bad-password",
        PersonalFileError::Pkcs12(P12Error::Malformed(_)) => "settings-certs-file-not-p12",
        PersonalFileError::Pkcs12(P12Error::Unsupported(_)) => "settings-certs-file-unsupported",
        PersonalFileError::Pkcs12(P12Error::NoKey) => "settings-certs-file-no-key",
    }
}

/// Состояние файлов процесса: итог по каждому пути и пароли, введённые в этом запуске.
#[derive(Default)]
pub(crate) struct Files {
    status: Vec<(String, FileStatus)>,
    session: HashMap<String, Zeroizing<String>>,
}

impl Global for Files {}

/// Итог по путям (для настроек).
pub(crate) fn status(cx: &App) -> Vec<(String, FileStatus)> {
    cx.try_global::<Files>().map(|f| f.status.clone()).unwrap_or_default()
}

/// Открыть файлы из настроек: пароль — введённый в этом запуске, иначе из хранилища; без пароля — «нужен пароль».
/// В фоне (хранилище и разбор блокируют поток); итог — в [`Files`], окна перерисовываются.
pub(crate) fn load(cx: &mut App) {
    let paths = crate::prefs::get(cx).security.smime_files;
    let session: HashMap<String, Zeroizing<String>> = cx.try_global::<Files>().map(|f| f.session.clone()).unwrap_or_default();
    let task = cx.background_spawn(async move {
        let mut with_password = Vec::new();
        let mut status = Vec::new();
        for path in &paths {
            let password = match session.get(path) {
                Some(p) => Some(p.clone()),
                None => match crate::sys::credentials::read(&credential_target(path)) {
                    Ok(p) => p.map(Zeroizing::new),
                    Err(e) => {
                        tracing::warn!("smime: certificate file password not read from the credential store: {e}");
                        None
                    }
                },
            };
            match password {
                Some(p) => with_password.push((std::path::PathBuf::from(path), p)),
                None => status.push((path.clone(), FileStatus::NeedPassword)),
            }
        }
        let results = postman_core::smime::load_personal_files(&with_password);
        for ((path, _), r) in with_password.iter().zip(results) {
            let s = match r {
                Ok(n) => FileStatus::Open(n),
                Err(e) => FileStatus::Failed(reason(&e)),
            };
            status.push((path.to_string_lossy().into_owned(), s));
        }
        // Порядок — как в настройках.
        status.sort_by_key(|(p, _)| paths.iter().position(|x| x == p));
        status
    });
    cx.spawn(async move |cx| {
        let status = task.await;
        cx.update(|cx| {
            let mut files = cx.try_global::<Files>().map(|f| Files { status: Vec::new(), session: f.session.clone() }).unwrap_or_default();
            files.status = status;
            cx.set_global(files);
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Поля «Свои сертификаты» для файлов.
pub(crate) struct FileUi {
    password: Entity<InputState>,
    /// Для какого файла спрашивается пароль (новый или «нужен пароль»).
    asking: Option<String>,
    /// Причина последней неудачи (ключ Fluent).
    error: Option<&'static str>,
    busy: bool,
    /// Убрать файл — ждёт подтверждения строкой.
    remove: Option<String>,
}

impl FileUi {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<SettingsView>, subscriptions: &mut Vec<Subscription>) -> Self {
        let password = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-certs-file-password")).masked(true));
        subscriptions.push(cx.subscribe_in(&password, window, |this: &mut SettingsView, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.open_smime_file(window, cx);
            }
        }));
        Self { password, asking: None, error: None, busy: false, remove: None }
    }
}

impl SettingsView {
    /// «Добавить из файла…»: выбор файла (портал системы), затем пароль строкой ниже.
    fn pick_smime_file(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut picked))) = paths.await else { return };
            let Some(path) = picked.pop() else { return };
            let _ = this.update(cx, |v, cx| {
                v.files.asking = Some(path.to_string_lossy().into_owned());
                v.files.error = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Открыть файл введённым паролем: годен — путь в настройки, пароль в хранилище (нет хранилища — до выхода),
    /// ключи заново; нет — причина строкой.
    fn open_smime_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.files.asking.clone() else { return };
        let password = Zeroizing::new(self.files.password.read(cx).value().to_string());
        if password.is_empty() || self.files.busy {
            return;
        }
        self.files.password.update(cx, |s, cx| s.set_value("", window, cx));
        self.files.busy = true;
        let check = {
            let (path, password) = (path.clone(), password.clone());
            cx.background_spawn(async move {
                postman_core::smime::check_personal_file(std::path::Path::new(&path), &password)?;
                if let Err(e) = crate::sys::credentials::write(&credential_target(&path), "", &password) {
                    // ADR 0011 п. 4: хранилища нет — пароль не на диск, спросим при следующем запуске.
                    tracing::warn!("smime: certificate file password not saved to the credential store: {e}");
                }
                Ok::<(), PersonalFileError>(())
            })
        };
        cx.spawn(async move |this, cx| {
            let result = check.await;
            let _ = this.update(cx, |v, cx| {
                v.files.busy = false;
                match result {
                    Ok(()) => {
                        v.files.asking = None;
                        v.files.error = None;
                        crate::prefs::update(cx, |p| {
                            if !p.security.smime_files.contains(&path) {
                                p.security.smime_files.push(path.clone());
                            }
                        });
                        let mut files =
                            cx.try_global::<Files>().map(|f| Files { status: f.status.clone(), session: f.session.clone() }).unwrap_or_default();
                        files.session.insert(path, password);
                        cx.set_global(files);
                        load(cx);
                        v.certs_asked = false;
                        v.certs = None;
                    }
                    Err(e) => v.files.error = Some(reason(&e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Убрать файл: из настроек, пароль — из хранилища, ключи заново.
    fn remove_smime_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.files.remove = None;
        crate::prefs::update(cx, |p| p.security.smime_files.retain(|x| *x != path));
        if let Some(files) = cx.try_global::<Files>() {
            let mut next = Files { status: files.status.clone(), session: files.session.clone() };
            next.session.remove(&path);
            next.status.retain(|(p, _)| *p != path);
            cx.set_global(next);
        }
        let target = credential_target(&path);
        cx.background_spawn(async move {
            if let Err(e) = crate::sys::credentials::delete(&target) {
                tracing::warn!("smime: certificate file password not removed from the credential store: {e}");
            }
        })
        .detach();
        load(cx);
        self.certs_asked = false;
        self.certs = None;
        cx.notify();
    }

    /// Строки файлов под списком своих сертификатов: файл и его состояние, ввод пароля, «Добавить из файла…».
    pub(crate) fn smime_file_rows(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let mut d = div().flex().flex_col();
        for (path, st) in status(cx) {
            let name = std::path::Path::new(&path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            let state = match st {
                FileStatus::Open(n) => tr!("settings-certs-file-open", n = n as i64),
                FileStatus::NeedPassword => tr!("settings-certs-file-need-password"),
                FileStatus::Failed(key) => tr!(key),
            };
            let control = if self.files.remove.as_deref() == Some(path.as_str()) {
                let yes = path.clone();
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_color(l.text2()).child(tr!("settings-certs-file-remove-confirm")))
                    .child(
                        button(("smime-file-remove-yes", 0usize), tr!("settings-certs-file-remove-yes"), true, l)
                            .on_click(cx.listener(move |v, _, _, cx| v.remove_smime_file(yes.clone(), cx))),
                    )
                    .child(button(("smime-file-remove-no", 0usize), tr!("settings-certs-file-remove-no"), false, l).on_click(cx.listener(
                        |v, _, _, cx| {
                            v.files.remove = None;
                            cx.notify();
                        },
                    )))
            } else {
                let (ask, rm) = (path.clone(), path.clone());
                div()
                    .flex()
                    .gap_2()
                    .when(st == FileStatus::NeedPassword || st == FileStatus::Failed("settings-certs-file-bad-password"), |d| {
                        d.child(button(("smime-file-password", 0usize), tr!("settings-certs-file-enter-password"), false, l).on_click(cx.listener(
                            move |v, _, _, cx| {
                                v.files.asking = Some(ask.clone());
                                v.files.error = None;
                                cx.notify();
                            },
                        )))
                    })
                    .child(button(("smime-file-remove", 0usize), tr!("settings-certs-file-remove"), false, l).on_click(cx.listener(
                        move |v, _, _, cx| {
                            v.files.remove = Some(rm.clone());
                            cx.notify();
                        },
                    )))
            };
            d = d.child(row(name, Some(state), control, l));
        }
        if let Some(path) = self.files.asking.clone() {
            let name = std::path::Path::new(&path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            let hint = self.files.error.map_or_else(|| tr!("settings-certs-file-password-hint"), |k| tr!(k));
            let control = div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().w(px(220.)).child(Input::new(&self.files.password)))
                .child(
                    button("smime-file-open", tr!("settings-certs-file-open-button"), true, l)
                        .on_click(cx.listener(|v, _, window, cx| v.open_smime_file(window, cx))),
                )
                .child(button("smime-file-cancel", tr!("settings-certs-file-cancel"), false, l).on_click(cx.listener(|v, _, _, cx| {
                    v.files.asking = None;
                    v.files.error = None;
                    cx.notify();
                })));
            d = d.child(row(tr!("settings-certs-file-password-for", name = name), Some(hint), control, l));
        }
        d.child(row(
            tr!("settings-certs-file-add"),
            Some(tr!("settings-certs-file-add-hint")),
            button("smime-file-add", tr!("settings-certs-file-add-button"), false, l).on_click(cx.listener(|v, _, _, cx| v.pick_smime_file(cx))),
            l,
        ))
    }
}

#[cfg(test)]
mod tests {
    use postman_core::smime::PersonalFileError;
    use postman_smime::p12::P12Error;

    use super::{credential_target, reason};

    /// Причина — один ключ Fluent; ключи есть в обоих языках.
    #[test]
    fn reasons_have_texts() {
        let all = [
            PersonalFileError::Read("x".into()),
            PersonalFileError::Pkcs12(P12Error::BadPassword),
            PersonalFileError::Pkcs12(P12Error::Malformed("x")),
            PersonalFileError::Pkcs12(P12Error::Unsupported("x".into())),
            PersonalFileError::Pkcs12(P12Error::NoKey),
        ];
        let keys: Vec<&str> = all.iter().map(reason).collect();
        let mut uniq = keys.clone();
        uniq.dedup();
        assert_eq!(uniq.len(), keys.len());
        for lang in ["ru", "en"] {
            let ftl = std::fs::read_to_string(format!("{}/i18n/{lang}/settings.ftl", env!("CARGO_MANIFEST_DIR"))).unwrap();
            for k in &keys {
                assert!(ftl.contains(&format!("{k} =")), "{lang}: {k}");
            }
        }
        assert_eq!(credential_target("/home/u/me.p12"), "Postman:smime:/home/u/me.p12");
    }
}
