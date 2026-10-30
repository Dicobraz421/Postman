//! «Настройки → Просмотр вложений» и модули просмотра в интерфейсе (S8-27, ADR 0007, макет S8-D «Настройки»).
//!
//! Реестр модулей (`postman_core::viewers::Registry`) читается один раз при старте в фоне; выключенные модули —
//! `prefs.viewers.disabled`. Выбор просмотра вложения ([`viewer`]) — встроенный главнее, нет его — модуль. Модуль
//! показывает хост `postman-viewer` в песочнице (процесс — по первому просмотру); ответ — разделами, как документ.
//!
//! Раздел настроек: встроенные форматы справкой, модули — по одному в строке (имя и версия, «сторонний» или «от
//! администратора», форматы и папка, выключатель), «Открыть папку модулей». Модули без описания или с импортами в
//! реестр не попадают (журнал).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_core::attach::{self, Viewer};
use postman_core::viewers::{ModuleId, Registry};
use postman_worker::Client;
use postman_worker::wasm::ViewerOutput;

use crate::reading::ReadingPane;
use crate::roles::Look;
use crate::settings_tab::{SettingsView, group, toggle};

/// Значок модуля (Segoe Fluent Icons «Пазл»).
const PUZZLE: char = '\u{EA86}';

/// Модули и выключенные — общие для ленты вложений и настроек.
#[derive(Default)]
struct State {
    registry: Arc<Registry>,
    off: BTreeSet<String>,
}

static STATE: RwLock<Option<State>> = RwLock::new(None);
/// Клиент хоста модулей: процесс — по первому просмотру, после выхода — новый на следующем.
static HOST: Mutex<Option<Client>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&State) -> T) -> T {
    let guard = STATE.read().unwrap_or_else(PoisonError::into_inner);
    match guard.as_ref() {
        Some(s) => f(s),
        None => f(&State::default()),
    }
}

fn set(f: impl FnOnce(&mut State)) {
    let mut guard = STATE.write().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(State::default));
}

/// Выключенные модули (из настроек при старте и после выключателя).
pub(crate) fn set_off(off: BTreeSet<String>) {
    set(|s| s.off = off);
}

/// Реестр модулей (при старте — из каталогов, в снимках — свой).
pub(crate) fn set_registry(r: Registry) {
    set(|s| s.registry = Arc::new(r));
}

/// Прочитать каталоги модулей в фоне (при старте): затем ленты вложений увидят модули.
pub(crate) fn start(cx: &mut App) {
    set_off(crate::prefs::get(cx).viewers.disabled.clone());
    let task = cx.background_spawn(async move { Registry::load(&postman_core::viewers::dirs()) });
    cx.spawn(async move |_| set_registry(task.await)).detach();
}

/// Просмотр вложения: встроенный главнее, нет его — модуль (выключенные — как нет их).
pub(crate) fn viewer(name: &str, mime: &str) -> Viewer {
    with(|s| attach::viewer_with(name, mime, b"", &s.registry, &s.off))
}

/// Подходящий, но выключенный модуль: его id и имя («Включить модуль «FB2»» на карточке «нет просмотра»).
pub(crate) fn disabled_for(name: &str, mime: &str) -> Option<(String, String)> {
    with(|s| {
        if s.off.is_empty() || attach::viewer_with(name, mime, b"", &s.registry, &BTreeSet::new()) == Viewer::None {
            return None;
        }
        let Viewer::Module { id } = attach::viewer_with(name, mime, b"", &s.registry, &BTreeSet::new()) else { return None };
        s.registry.get(id).filter(|m| s.off.contains(&m.id)).map(|m| (m.id.clone(), m.description.name.clone()))
    })
}

/// Подпись в шапке просмотра модулем: «Просмотр: модуль «FB2» 1.0», у стороннего — «(сторонний) · в песочнице».
pub(crate) fn module_note(id: ModuleId) -> String {
    with(|s| match s.registry.get(id) {
        Some(m) if m.third_party => tr!("att-pv-module-third", name = m.description.name.as_str(), version = m.description.version.as_str()),
        Some(m) => tr!("att-pv-module", name = m.description.name.as_str(), version = m.description.version.as_str()),
        None => String::new(),
    })
}

/// Включить или выключить модуль `id`: настройки и выбор просмотра.
fn switch(id: &str, on: bool, cx: &mut App) {
    crate::prefs::update(cx, |p| {
        if on {
            p.viewers.disabled.remove(id);
        } else {
            p.viewers.disabled.insert(id.to_owned());
        }
    });
    set_off(crate::prefs::get(cx).viewers.disabled.clone());
}

/// Папка модулей пользователя: создать, если нет, и открыть.
fn open_folder() -> Result<(), String> {
    let dir = postman_core::viewers::dirs().into_iter().find(|d| d.third_party).ok_or("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir.path).map_err(|e| format!("{}: {e}", dir.path.display()))?;
    crate::sys::shell::open(&dir.path.to_string_lossy())
}

impl ReadingPane {
    /// Модуль просмотра (S8-27): байты — хосту `postman-viewer` в фоне, ответ — разделами (или картинкой).
    pub(crate) fn run_module(&mut self, cx: &mut Context<Self>) {
        use crate::preview::{PvState, Shown};
        let Some(r) = &self.reading else { return };
        let Some(p) = &r.preview else { return };
        let (Viewer::Module { id }, Some(data)) = (p.viewer, p.data.clone()) else { return };
        let Some((wasm, name, version, third)) =
            with(|s| s.registry.get(id).map(|m| (m.wasm.clone(), m.description.name.clone(), m.description.version.clone(), m.third_party)))
        else {
            return;
        };
        let (letter, blob) = (r.row.id.clone(), p.row.blob_id.clone());
        let task = cx.background_spawn(async move {
            let mut host = HOST.lock().unwrap_or_else(PoisonError::into_inner);
            let client = host.get_or_insert_with(|| Client::viewer(Client::default_viewer_exe()));
            client.view_module(&wasm, &data)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(pv) = this.reading.as_mut().filter(|r| r.row.id == letter).and_then(|r| r.preview.as_mut()) else { return };
                if pv.row.blob_id != blob {
                    return;
                }
                pv.busy = false;
                pv.state = match result {
                    Ok(Ok(ViewerOutput::Sections { sections, truncated })) => PvState::Ready(Shown::Sections {
                        layout: crate::sections::Layout::Doc,
                        sections,
                        truncated,
                        at: 0,
                        module: Some((name, version, third)),
                    }),
                    Ok(Ok(ViewerOutput::Image { width, height, rgba })) => {
                        match crate::reading::render_image(postman_worker::Rgba { width, height, pixels: rgba }) {
                            Ok(image) => PvState::Ready(Shown::Image { image, source: (width, height), format: name }),
                            Err(e) => PvState::Failed(e),
                        }
                    }
                    Ok(Ok(ViewerOutput::Error(text))) => PvState::Failed(text),
                    Ok(Err(refusal)) => PvState::Failed(tr!("att-pv-module-refused", reason = format!("{refusal:?}"))),
                    Err(e) => PvState::Failed(e),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Карточка «нет просмотра»: подходящий модуль выключен — «Включить модуль «…»».
    pub(crate) fn enable_module_link(&self, name: &str, mime: &str, l: &Look, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let (id, module) = disabled_for(name, mime)?;
        let blob = self.reading.as_ref().and_then(|r| r.preview.as_ref()).map(|p| p.row.blob_id.clone());
        Some(crate::settings_tab::link("pv-enable-module", tr!("att-pv-module-enable", name = module.as_str()), l).on_click(cx.listener(
            move |this, _, _, cx| {
                switch(&id, true, cx);
                // Тот же файл — заново, уже модулем.
                this.close_preview(cx);
                this.toggle_preview(blob.clone(), cx);
            },
        )))
    }
}

/// Строки встроенных форматов (справка, без выключателей).
const BUILTIN: [(&str, &str); 6] = [
    ("settings-viewers-docs", "PDF, Word (docx), OpenDocument (odt), RTF, Markdown"),
    ("settings-viewers-sheets", "Excel (xlsx), OpenDocument (ods), CSV, TSV"),
    ("settings-viewers-slides", "PowerPoint (pptx), OpenDocument (odp)"),
    ("settings-viewers-images", "PNG, JPEG, GIF, BMP, ICO, TIFF"),
    ("settings-viewers-mail", "EML, MSG"),
    ("settings-viewers-archives", ""),
];

impl SettingsView {
    /// Раздел «Просмотр вложений».
    pub(crate) fn viewers(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let builtin = div().flex().flex_col().gap(px(6.)).py_2().children(BUILTIN.iter().map(|(key, formats)| {
            let formats = if formats.is_empty() { tr!("settings-viewers-zip") } else { (*formats).to_owned() };
            div()
                .flex()
                .gap_4()
                .child(div().w(px(140.)).flex_none().text_color(l.text2()).child(tr!(*key)))
                .child(div().flex_1().min_w_0().child(formats))
        }));
        let (modules, off) = with(|s| (s.registry.modules().map(|(_, m)| m.clone()).collect::<Vec<_>>(), s.off.clone()));
        let dirs = postman_core::viewers::dirs();
        let list = if modules.is_empty() {
            div().py_3().text_color(l.text2()).child(tr!("settings-viewers-none")).into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(modules.into_iter().map(|m| {
                    let on = !off.contains(&m.id);
                    let mark = if m.third_party { tr!("settings-viewers-third") } else { tr!("settings-viewers-admin") };
                    let mut formats: Vec<String> = m.description.extensions.clone();
                    if formats.is_empty() {
                        formats = m.description.mime.clone();
                    }
                    let dir = dirs.iter().find(|d| d.third_party == m.third_party).map(|d| d.path.display().to_string()).unwrap_or_default();
                    let id = m.id.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_3()
                        .border_b_1()
                        .border_color(l.border())
                        .child(crate::chrome::icon(PUZZLE, 16.).text_color(l.text2()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(format!("{} · {}", m.description.name, m.description.version))
                                        .child(div().px(px(6.)).rounded(px(4.)).bg(l.fill()).text_size(px(11.)).text_color(l.text2()).child(mark)),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(l.px(Metric::TypeCaption))
                                        .text_color(l.text2())
                                        .child(format!("{} · {dir}", formats.join(", "))),
                                ),
                        )
                        .child(toggle(SharedString::from(format!("viewer-{}", m.id)), on, l).on_click(cx.listener(move |_, _, _, cx| {
                            switch(&id, !on, cx);
                            cx.notify();
                        })))
                }))
                .into_any_element()
        };
        let folder = crate::buttons::button("settings-viewers-open", tr!("settings-viewers-open"), false, l).on_click(cx.listener(|_, _, _, cx| {
            if let Err(e) = open_folder() {
                tracing::warn!("viewer modules folder: {e}");
            }
            cx.notify();
        }));
        div()
            .flex()
            .flex_col()
            .child(group(tr!("settings-viewers-builtin"), l))
            .child(builtin)
            .child(group(tr!("settings-viewers-modules"), l))
            .child(list)
            .child(
                div()
                    .pt_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(folder)
                    .child(div().flex_1().min_w_0().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("settings-viewers-tip"))),
            )
    }
}

/// Опись части (ADR 0005, `parts.rs`): сцены раздела и просмотра модулем.
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "viewers", scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Снимки: модули — в своей временной папке (описание образца FB2 и «от администратора» — DjVu); хост в снимке не
/// запускается — ответ модуля подставлен (`screen_module`), как его отдал бы `postman-viewer`.
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::Mailer;
    use crate::screens::{self, Scene};
    use crate::settings_tab::Section as S;

    pub(crate) const SCENES: &[Scene] = &[
        // «Настройки → Просмотр вложений»: встроенные форматы и два модуля.
        Scene::new("settings-viewers", settings, settings_ready),
        // Книга FB2 модулем: подпись «Просмотр: модуль «FB2» 1.0 (сторонний) · в песочнице».
        Scene::new("preview-module", module, module_ready).wait(module_wait),
    ];

    /// Реестр снимков: FB2 (сторонний) и DjVu (от администратора) — описания и заголовок WASM.
    fn registry() -> Result<postman_core::viewers::Registry, String> {
        let root = postman_demo::testdata_dir().join("screens-viewers");
        let (user, system) = (root.join("user"), root.join("system"));
        let wasm = b"\0asm\x01\0\0\0";
        let djvu = r#"{"name": "DjVu", "version": "0.3", "abi": 1, "extensions": ["djvu"], "mime": ["image/vnd.djvu"]}"#;
        for (dir, name, json) in [(&user, "fb2", include_str!("../../viewers/fb2/fb2.json")), (&system, "djvu", djvu)] {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join(format!("{name}.json")), json).map_err(|e| e.to_string())?;
            std::fs::write(dir.join(format!("{name}.wasm")), wasm).map_err(|e| e.to_string())?;
        }
        let dirs = [postman_core::viewers::Dir { path: system, third_party: false }, postman_core::viewers::Dir { path: user, third_party: true }];
        Ok(postman_core::viewers::Registry::load(&dirs))
    }

    fn settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        super::set_registry(registry()?);
        m.open_settings(Some(S::Viewers), window, cx);
        Ok(())
    }

    fn settings_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::settings_on(m, S::Viewers, cx)
    }

    fn module(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        super::set_registry(registry()?);
        m.set_section(crate::Section::Mail, cx);
        if screens::reading(m, cx).is_none() {
            screens::select_demo(m, screens::outlook_letter, cx)?;
        }
        Ok(())
    }

    /// Письмо открылось — книга модулем в просмотр.
    fn module_wait(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        let Some(r) = screens::reading(m, cx) else { return };
        if r.preview.as_ref().is_some_and(|p| p.row.blob_id == BOOK_BLOB) {
            return;
        }
        if let Some(id) = super::with(|s| s.registry.modules().find(|(_, m)| m.id == "fb2").map(|(id, _)| id)) {
            m.reading.update(cx, |r, cx| r.screen_module(BOOK, id, cx));
        }
    }

    const BOOK: &str = "Мастер и Маргарита.fb2"; // i18n-ok: имя файла образца
    const BOOK_BLOB: &str = "screen-Мастер и Маргарита.fb2"; // i18n-ok: имя файла образца

    fn module_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::reading(m, cx).and_then(|r| r.preview.as_ref()).is_some_and(|p| p.ready() && p.row.blob_id == BOOK_BLOB)
    }
}

impl ReadingPane {
    /// Снимок `preview-module`: просмотр модулем с ответом, как от `postman-viewer` (глава книги).
    pub(crate) fn screen_module(&mut self, name: &str, id: ModuleId, cx: &mut Context<Self>) {
        use crate::preview::{PvState, Shown};
        use postman_worker::{Section, SectionTitle};
        let sections = vec![Section { title: SectionTitle::None, blocks: crate::sections::samples::fb2_chapter() }];
        let (mname, version, third) =
            with(|s| s.registry.get(id).map(|m| (m.description.name.clone(), m.description.version.clone(), m.third_party))).unwrap_or_default();
        self.screen_pv(name, postman_core::attach::Viewer::Module { id }, None);
        if let Some(p) = self.reading.as_mut().and_then(|r| r.preview.as_mut()) {
            p.busy = false;
            p.state = PvState::Ready(Shown::Sections {
                layout: crate::sections::Layout::Doc,
                sections,
                truncated: false,
                at: 0,
                module: Some((mname, version, third)),
            });
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use postman_core::attach::Viewer;
    use postman_core::viewers::{Dir, Registry};

    /// Выбор просмотра (S8-27): встроенный главнее, нет его — модуль; выключенный — как нет его, и карточка «нет
    /// просмотра» предлагает его включить.
    #[test]
    fn modules_and_disabled() {
        let dir = tempfile::Builder::new().prefix("postman-viewers-").tempdir().unwrap();
        std::fs::write(dir.path().join("fb2.json"), include_str!("../../viewers/fb2/fb2.json")).unwrap();
        std::fs::write(dir.path().join("fb2.wasm"), b"\0asm\x01\0\0\0").unwrap();
        super::set_registry(Registry::load(&[Dir { path: dir.path().to_owned(), third_party: true }]));
        super::set_off(BTreeSet::new());
        assert!(matches!(super::viewer("Книга.fb2", ""), Viewer::Module { .. }));
        assert_eq!(super::viewer("Итоги.xlsx", ""), Viewer::Sheet, "встроенный главнее");
        assert_eq!(super::disabled_for("Книга.fb2", ""), None);
        super::set_off(BTreeSet::from(["fb2".to_owned()]));
        assert_eq!(super::viewer("Книга.fb2", ""), Viewer::None);
        assert_eq!(super::disabled_for("Книга.fb2", ""), Some(("fb2".into(), "FB2".into())));
        super::set_off(BTreeSet::new());
    }
}
