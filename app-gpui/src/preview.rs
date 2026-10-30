//! Встроенный просмотр вложения (S5-07, макет «Просмотр вложений»): панель на месте текста письма в области
//! чтения, не окно. Esc — закрыть, ← / → — соседнее вложение, у PDF — страницы.
//!
//! Файл разбирает только воркер-песочница (`postman_worker::Client::preview`): картинка и страница PDF приходят
//! пикселями, архив — оглавлением (без распаковки), текст — строкой, документ Office — итогом проверки на макросы.
//! Здесь же — проверка документов Office при открытии письма (щиток в ленте) и «Сохранить все».

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use pets_ui::theme::Metric;
use postman_cache::AttachmentRow;
use postman_core::attach::{self, Risk, Viewer};
use postman_core::files;
use postman_worker::{Listing, OfficeScan, Preview, PreviewKind, preview::MAX_INPUT};

use crate::attachments::{self, FileAct, glyph, size_text};
use crate::chrome::icon;
use crate::reading::{ReadingEvent, ReadingPane, render_image};
use crate::roles::{Look, Pm};

/// Длинная сторона картинки и страницы PDF в пикселях. Кадр живёт дважды: пиксели в `RenderImage` и текстура атласа
/// gpui (на встроенной графике — тоже память процесса): 2048 × 2048 RGBA — 16 МиБ, вместе ~32 МиБ, в запасе бюджета
/// 150 МБ (`docs/HANDOFF.md` §2c, пик 105–115 МБ); 4096 — уже ~128 МиБ. Фото 4:3 — 2048 × 1536, ~24 МиБ.
const IMAGE_SIDE: u32 = 2048;
const PDF_SIDE: u32 = 2048;
/// Ширина страницы PDF в пикселях, пока ширина панели не известна (первый показ).
const PDF_FALLBACK_W: u32 = 1000;
/// Поля холста просмотра.
const CANVAS_PAD: f32 = 16.;
/// Документы Office больше не проверяем при открытии письма (только по «Просмотр»).
const SCAN_MAX: u64 = 25 * 1024 * 1024;

/// Открытый просмотр.
pub(crate) struct Pv {
    pub(crate) row: AttachmentRow,
    pub(crate) name: String,
    pub(crate) viewer: Viewer,
    pub(crate) risk: Risk,
    /// Страница PDF (с нуля), которую показываем или ждём.
    pub(crate) page: u32,
    /// Байты файла: страницы PDF листаются без нового скачивания.
    pub(crate) data: Option<Arc<Vec<u8>>>,
    pub(crate) state: PvState,
    /// Идёт разбор следующей страницы: прежняя ещё видна.
    pub(crate) busy: bool,
    /// Прокрутка холста картинки и PDF; её `bounds` — ширина холста для страницы PDF.
    pub(crate) scroll: ScrollHandle,
    /// Ширина страницы PDF в пикселях, с которой её просили у воркера (0 — ещё не просили).
    pub(crate) fit_w: u32,
}

pub(crate) enum PvState {
    Loading,
    Ready(Shown),
    Failed(String),
}

pub(crate) enum Shown {
    Image {
        image: Arc<RenderImage>,
        source: (u32, u32),
        format: String,
    },
    Pdf {
        image: Arc<RenderImage>,
        page: u32,
        pages: u32,
        title: Option<String>,
    },
    Archive(Listing),
    Text {
        text: String,
        truncated: bool,
    },
    Office(OfficeScan),
    /// Документ разделами (T1-13в, S8-23): книга — листами, презентация — по слайду, прочее — подряд; `at` — открытый
    /// лист или слайд; `module` — показал модуль просмотра (S8-27): имя, версия, сторонний.
    Sections {
        layout: crate::sections::Layout,
        sections: Vec<postman_worker::Section>,
        truncated: bool,
        at: usize,
        module: Option<(String, String, bool)>,
    },
    /// Письмо во вложении (S8-23).
    Letter(Box<crate::sections::Letter>),
    /// Встроенного просмотра нет: карточка (исполняемый файл — блокировка).
    Card,
}

impl Pv {
    /// Разобран (или ошибка): снимок можно делать.
    pub(crate) fn ready(&self) -> bool {
        !matches!(self.state, PvState::Loading) && !self.busy
    }

    /// Кадр в атласе gpui — убрать при закрытии или замене.
    pub(crate) fn image(&self) -> Option<Arc<RenderImage>> {
        match &self.state {
            PvState::Ready(Shown::Image { image, .. } | Shown::Pdf { image, .. }) => Some(image.clone()),
            _ => None,
        }
    }

    /// Открытый лист или слайд.
    pub(crate) fn section(&self) -> Option<usize> {
        match &self.state {
            PvState::Ready(Shown::Sections { at, .. }) => Some(*at),
            _ => None,
        }
    }

    /// Разбирает воркер или модуль просмотра: байты нужны.
    fn wants_data(&self) -> bool {
        self.kind().is_some() || matches!(self.viewer, Viewer::Module { .. })
    }

    fn kind(&self) -> Option<PreviewKind> {
        match self.viewer {
            Viewer::Image => Some(PreviewKind::Image),
            Viewer::Pdf => Some(PreviewKind::Pdf),
            Viewer::Archive => Some(PreviewKind::Archive),
            Viewer::Text => Some(PreviewKind::Text),
            // Документ Word — текстом IR (T1-13в); с макросами воркер отвечает проверкой Office.
            Viewer::Document => Some(PreviewKind::Document),
            // Листы, таблицы, слайды, письмо во вложении (S8-23) — запросом ядра.
            v @ (Viewer::Sheet | Viewer::Table | Viewer::Slides | Viewer::Letter) => v.kind(),
            // Модуль просмотра (S8-27) — хостом `postman-viewer`, не воркером.
            Viewer::Office | Viewer::Module { .. } | Viewer::None => None,
        }
    }
}

/// «Сохранить все»: каталог и байты, которых ждём.
pub(crate) struct SaveAll {
    dir: PathBuf,
    waiting: HashSet<String>,
    got: Vec<(String, Arc<Vec<u8>>)>,
    failed: Vec<(String, String)>,
}

impl ReadingPane {
    /// Щелчок по карточке или «Просмотр»: открыть, а для уже открытого — закрыть. `None` — первое вложение.
    pub(crate) fn toggle_preview(&mut self, blob: Option<String>, cx: &mut Context<Self>) {
        let Some(r) = &self.reading else { return };
        let current = r.preview.as_ref().map(|p| p.row.blob_id.clone());
        let target = blob.or_else(|| attachments::items(r).first().map(|i| i.row.blob_id.clone()));
        match target {
            Some(t) if current.as_deref() != Some(t.as_str()) => self.open_preview(&t, cx),
            _ => {
                self.close_preview(cx);
            }
        }
    }

    /// Esc: закрыть просмотр; `false` — его не было.
    pub(crate) fn close_preview(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(p) = self.reading.as_mut().and_then(|r| r.preview.take()) else { return false };
        self.retired.extend(p.image());
        cx.notify();
        true
    }

    /// ← / → в просмотре: у презентации — соседний слайд (как страницы PDF), иначе — соседнее вложение; `false` —
    /// просмотр не открыт.
    pub(crate) fn step_preview(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        if self.step_slide(delta, cx) {
            return true;
        }
        let Some(r) = &self.reading else { return false };
        let Some(current) = r.preview.as_ref().map(|p| p.row.blob_id.clone()) else { return false };
        let items = attachments::items(r);
        let Some(i) = items.iter().position(|i| i.row.blob_id == current) else { return true };
        let next = i.saturating_add_signed(delta).min(items.len().saturating_sub(1));
        if next != i {
            let blob = items[next].row.blob_id.clone();
            self.open_preview(&blob, cx);
        }
        true
    }

    pub(crate) fn set_expanded(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(r) = &mut self.reading {
            r.expanded = on;
            cx.notify();
        }
    }

    fn open_preview(&mut self, blob: &str, cx: &mut Context<Self>) {
        let Some(r) = &mut self.reading else { return };
        let Some(item) = attachments::items(r).into_iter().find(|i| i.row.blob_id == blob) else { return };
        let mut pv = Pv {
            row: item.row.clone(),
            name: item.name.clone(),
            viewer: item.viewer,
            risk: item.risk,
            page: 0,
            data: None,
            state: PvState::Loading,
            busy: false,
            scroll: ScrollHandle::new(),
            // Ширина панели — от прежнего просмотра: страницу не придётся перерисовывать.
            fit_w: r.preview.as_ref().map_or(0, |p| p.fit_w),
        };
        // Карточка за «+N» — лента разворачивается, выбранное видно.
        let index = attachments::items(r).iter().position(|i| i.row.blob_id == blob).unwrap_or(0);
        if index >= attachments::visible(attachments::items(r).len(), false).0 {
            r.expanded = true;
        }
        let mut fetch = false;
        match item.viewer {
            Viewer::None => pv.state = PvState::Ready(Shown::Card),
            Viewer::Office => match r.scans.get(blob) {
                Some(s) => pv.state = PvState::Ready(Shown::Office(s.clone())),
                None => {
                    r.scanning.insert(blob.to_string());
                    fetch = true;
                }
            },
            _ if item.row.size > MAX_INPUT as u64 => {
                pv.state = PvState::Failed(tr!("att-pv-too-big", size = size_text(MAX_INPUT as u64)));
            }
            _ => fetch = true,
        }
        if let Some(old) = r.preview.replace(pv) {
            self.retired.extend(old.image());
        }
        if fetch {
            self.fetch(&item.row, cx);
        }
        cx.notify();
    }

    /// Документы Office открытого письма — на проверку макросов в воркер (байты — как для «Открыть»).
    pub(crate) fn ensure_scans(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &mut self.reading else { return };
        let wanted: Vec<AttachmentRow> = r
            .body
            .iter()
            .flat_map(|b| &b.attachments)
            .filter(|a| attach::needs_scan(&a.name) && a.size <= SCAN_MAX)
            .filter(|a| !r.scans.contains_key(&a.blob_id))
            .cloned()
            .collect();
        let wanted: Vec<AttachmentRow> = wanted.into_iter().filter(|a| r.scanning.insert(a.blob_id.clone())).collect();
        for a in wanted {
            self.fetch(&a, cx);
        }
    }

    /// Байты пришли: просмотр, проверка Office, «Сохранить все». `true` — их ждало действие пользователя.
    pub(crate) fn attachment_ready(&mut self, blob: &str, data: &Arc<Vec<u8>>, cx: &mut Context<Self>) -> bool {
        let mut user = false;
        if self.reading.as_mut().is_some_and(|r| r.scanning.remove(blob)) {
            self.run_scan(blob.to_string(), data.clone(), cx);
        }
        let shown = match self.reading.as_mut().and_then(|r| r.preview.as_mut()) {
            Some(p) if p.row.blob_id == blob && p.wants_data() && p.data.is_none() => {
                p.data = Some(data.clone());
                true
            }
            _ => false,
        };
        if shown {
            user = true;
            self.run_preview(cx);
        }
        if let Some(s) = &mut self.save_all
            && s.waiting.remove(blob)
        {
            let name = self.reading.as_ref().and_then(|r| r.body.as_ref()).and_then(|b| b.attachments.iter().find(|a| a.blob_id == blob));
            if let Some(a) = name {
                s.got.push((a.name.clone(), data.clone()));
            }
            user = true;
            self.finish_save_all(cx);
        }
        user
    }

    /// Байты не пришли. `true` — их ждало действие пользователя (нужен тост).
    pub(crate) fn attachment_failed(&mut self, blob: &str, error: &str, cx: &mut Context<Self>) -> bool {
        let mut user = false;
        if let Some(r) = &mut self.reading {
            r.scanning.remove(blob);
            if let Some(p) = r.preview.as_mut().filter(|p| p.row.blob_id == blob && p.data.is_none()) {
                p.state = PvState::Failed(error.to_string());
                user = true;
            }
        }
        if let Some(s) = &mut self.save_all
            && s.waiting.remove(blob)
        {
            s.failed.push((blob.to_string(), error.to_string()));
            user = true;
            self.finish_save_all(cx);
        }
        user
    }

    fn run_scan(&mut self, blob: String, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        let Some(id) = self.reading.as_ref().map(|r| r.row.id.clone()) else { return };
        let worker = self.worker.clone();
        let task = cx.background_spawn(async move {
            let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
            w.preview(PreviewKind::Office, &data, 0, 0, 0)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) else { return };
                let scan = match result {
                    Ok((Preview::Office(s), _)) => Ok(s),
                    Ok(_) => Err(tr!("att-pv-failed", error = "?")),
                    Err(e) => Err(e),
                };
                let waiting = r
                    .preview
                    .as_mut()
                    .filter(|p| p.row.blob_id == blob && matches!(p.viewer, Viewer::Office | Viewer::Document | Viewer::Sheet | Viewer::Slides));
                match scan {
                    Ok(s) => {
                        if let Some(p) = waiting {
                            p.risk = attach::risk_with(&p.row.name, Some(s.macros));
                            p.state = PvState::Ready(Shown::Office(s.clone()));
                        }
                        r.scans.insert(blob, s);
                    }
                    Err(e) => {
                        tracing::warn!("office scan failed: {e}");
                        if let Some(p) = waiting {
                            p.state = PvState::Failed(e);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn run_preview(&mut self, cx: &mut Context<Self>) {
        if self.reading.as_ref().and_then(|r| r.preview.as_ref()).is_some_and(|p| matches!(p.viewer, Viewer::Module { .. })) {
            return self.run_module(cx);
        }
        let Some(r) = &mut self.reading else { return };
        let Some(p) = &mut r.preview else { return };
        let layout = crate::sections::Layout::of(p.viewer);
        let (Some(kind), Some(data)) = (p.kind(), p.data.clone()) else { return };
        let (id, blob, page) = (r.row.id.clone(), p.row.blob_id.clone(), p.page);
        let (side, width) = match kind {
            PreviewKind::Pdf => {
                if p.fit_w == 0 {
                    p.fit_w = PDF_FALLBACK_W;
                }
                (PDF_SIDE, p.fit_w)
            }
            _ => (IMAGE_SIDE, 0),
        };
        let worker = self.worker.clone();
        let task = cx.background_spawn(async move {
            let result = {
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                w.preview(kind, &data, page, side, width)
            };
            // Пиксели — в кадр gpui здесь же, в фоне.
            result.and_then(|(p, rgba)| {
                let image = rgba.map(render_image).transpose()?;
                Ok::<_, String>((p, image))
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(pv) = this.reading.as_mut().filter(|r| r.row.id == id).and_then(|r| r.preview.as_mut()) else { return };
                if pv.row.blob_id != blob || pv.page != page {
                    return;
                }
                let old = pv.image();
                pv.busy = false;
                pv.state = match result {
                    Ok((Preview::Image { source, format, .. }, Some(image))) => PvState::Ready(Shown::Image { image, source, format }),
                    Ok((Preview::Pdf { page, pages, title, .. }, Some(image))) => {
                        pv.page = page;
                        PvState::Ready(Shown::Pdf { image, page, pages, title })
                    }
                    Ok((Preview::Archive(l), _)) => PvState::Ready(Shown::Archive(l)),
                    Ok((Preview::Text { text, truncated }, _)) => PvState::Ready(Shown::Text { text, truncated }),
                    Ok((Preview::Office(s), _)) => PvState::Ready(Shown::Office(s)),
                    Ok((Preview::Document { sections, truncated }, _)) => {
                        PvState::Ready(Shown::Sections { layout, sections, truncated, at: 0, module: None })
                    }
                    Ok((Preview::Letter { from, to, cc, date, subject, blocks, attachments, truncated }, _)) => {
                        PvState::Ready(Shown::Letter(Box::new(crate::sections::Letter {
                            from,
                            to,
                            cc,
                            date,
                            subject,
                            blocks,
                            attachments,
                            truncated,
                        })))
                    }
                    Ok(_) => PvState::Failed("?".into()),
                    Err(e) => PvState::Failed(e),
                };
                this.retired.extend(old);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Снимок `preview-docx`: документ Word из байтов (`docx` — собран сценой) — через воркер, как вложение.
    pub(crate) fn screen_document(&mut self, docx: Vec<u8>, cx: &mut Context<Self>) {
        self.screen_preview("report.docx", Viewer::Document, docx, cx);
    }

    /// Снимки просмотра (S8-23, S8-27): файл `name` из байтов (собран сценой) — тем же путём, что вложение.
    pub(crate) fn screen_preview(&mut self, name: &str, viewer: Viewer, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.screen_pv(name, viewer, Some(bytes));
        self.run_preview(cx);
        cx.notify();
    }

    /// Просмотр снимка без разбора: байты `data` (нет — состояние ставит сцена).
    pub(crate) fn screen_pv(&mut self, name: &str, viewer: Viewer, data: Option<Vec<u8>>) {
        let Some(r) = &mut self.reading else { return };
        let bytes = data.unwrap_or_default();
        let row = AttachmentRow {
            blob_id: format!("screen-{name}"),
            name: name.into(),
            mime: String::new(),
            size: bytes.len() as u64,
            cid: None,
            inline: false,
        };
        let pv = Pv {
            name: row.name.clone(),
            row,
            viewer,
            risk: Risk::Safe,
            page: 0,
            data: (!bytes.is_empty()).then(|| Arc::new(bytes)),
            state: PvState::Loading,
            busy: false,
            scroll: ScrollHandle::new(),
            fit_w: 0,
        };
        if let Some(old) = r.preview.replace(pv) {
            self.retired.extend(old.image());
        }
    }

    /// Лист или слайд `i` (вкладка, листание).
    pub(crate) fn screen_section(&mut self, i: usize, cx: &mut Context<Self>) {
        self.go_section(i, cx);
    }

    fn go_section(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(p) = self.reading.as_mut().and_then(|r| r.preview.as_mut()) else { return };
        if let PvState::Ready(Shown::Sections { sections, at, .. }) = &mut p.state
            && i < sections.len()
            && *at != i
        {
            *at = i;
            p.scroll = ScrollHandle::new();
            cx.notify();
        }
    }

    /// ← / → у презентации: соседний слайд; `false` — не презентация.
    fn step_slide(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(p) = self.reading.as_ref().and_then(|r| r.preview.as_ref()) else { return false };
        let PvState::Ready(Shown::Sections { layout: crate::sections::Layout::Slides, sections, at, .. }) = &p.state else { return false };
        let next = at.saturating_add_signed(delta).min(sections.len().saturating_sub(1));
        self.go_section(next, cx);
        true
    }

    /// Страница PDF: соседняя по кнопке.
    fn pdf_page(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(p) = self.reading.as_mut().and_then(|r| r.preview.as_mut()) else { return };
        let PvState::Ready(Shown::Pdf { page, pages, .. }) = &p.state else { return };
        let next = (i64::from(*page) + delta).clamp(0, i64::from(*pages) - 1) as u32;
        if next == p.page || p.busy {
            return;
        }
        p.page = next;
        p.busy = true;
        self.run_preview(cx);
        cx.notify();
    }

    /// Ширина холста изменилась заметно (первый показ, ресайз окна): страницу PDF — заново под ширину `want`.
    fn refit_pdf(&mut self, want: u32, cx: &mut Context<Self>) {
        let Some(p) = self.reading.as_mut().and_then(|r| r.preview.as_mut()) else { return };
        if p.busy || !matches!(p.state, PvState::Ready(Shown::Pdf { .. })) || !needs_refit(p.fit_w, want) {
            return;
        }
        p.fit_w = want;
        p.busy = true;
        self.run_preview(cx);
        cx.notify();
    }

    /// «Сохранить все…»: каталог — диалогом ОС (поток окна не ждёт), затем байты всех вложений ленты.
    pub(crate) fn save_all(&mut self, cx: &mut Context<Self>) {
        if self.save_all.is_some() {
            return;
        }
        let Some(r) = &self.reading else { return };
        let rows: Vec<AttachmentRow> = attachments::items(r).into_iter().map(|i| i.row).collect();
        if rows.is_empty() {
            return;
        }
        let id = r.row.id.clone();
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            let dir = match picked.await {
                Ok(Ok(Some(mut dirs))) if !dirs.is_empty() => dirs.swap_remove(0),
                Ok(Err(e)) => {
                    let text = tr!("att-save-all-failed", name = "…", error = e.to_string());
                    this.update(cx, |_, cx| cx.emit(ReadingEvent::Toast(Toast::Error, text))).ok();
                    return;
                }
                _ => return,
            };
            this.update(cx, |this, cx| {
                if this.reading.as_ref().is_none_or(|r| r.row.id != id) || this.save_all.is_some() {
                    return;
                }
                let waiting = rows.iter().map(|a| a.blob_id.clone()).collect();
                this.save_all = Some(SaveAll { dir, waiting, got: Vec::new(), failed: Vec::new() });
                for a in &rows {
                    this.fetch(a, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Все байты пришли — записать в фоне с меткой «из Интернета», не затирая файлы каталога.
    fn finish_save_all(&mut self, cx: &mut Context<Self>) {
        if self.save_all.as_ref().is_none_or(|s| !s.waiting.is_empty()) {
            return;
        }
        let Some(s) = self.save_all.take() else { return };
        cx.notify();
        let task = cx.background_spawn(async move {
            let mut taken: HashSet<String> =
                std::fs::read_dir(&s.dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect()).unwrap_or_default();
            let mut failed = s.failed;
            let mut saved = 0usize;
            for (name, data) in &s.got {
                let file = attach::unique_name(name, &mut taken);
                match files::write(&s.dir.join(&file), data) {
                    Ok(()) => saved += 1,
                    Err(e) => failed.push((file, e.to_string())),
                }
            }
            (s.dir, saved, failed)
        });
        cx.spawn(async move |this, cx| {
            let (dir, saved, failed) = task.await;
            this.update(cx, |_, cx| {
                if saved > 0 {
                    cx.emit(ReadingEvent::Toast(Toast::Success, tr!("att-saved-all", n = saved, dir = dir.display().to_string())));
                }
                for (name, error) in failed {
                    cx.emit(ReadingEvent::Toast(Toast::Error, tr!("att-save-all-failed", name = name, error = error)));
                }
            })
            .ok();
        })
        .detach();
    }

    // --- отрисовка ---

    /// Панель просмотра на месте текста письма.
    pub(crate) fn preview_pane(&self, p: &Pv, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let l = crate::roles::look(cx);
        let note = match (&p.state, p.viewer) {
            // Показал модуль просмотра (S8-27): имя и версия, сторонний — с пометкой.
            (PvState::Ready(Shown::Sections { module: Some((name, version, third)), .. }), _) => {
                let key = if *third { "att-pv-module-third" } else { "att-pv-module" };
                tr!(key, name = name.as_str(), version = version.as_str())
            }
            (_, Viewer::Module { id }) => crate::settings_viewers::module_note(id),
            (_, Viewer::Archive) => tr!("att-pv-note-archive"),
            (_, Viewer::Office | Viewer::None) => tr!("att-pv-note-check"),
            _ => tr!("att-pv-note-builtin"),
        };
        let header = div()
            .h(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(12.))
            .pr(px(4.))
            .border_b_1()
            .border_color(l.border())
            .child(div().flex_none().max_w(px(360.)).truncate().font_weight(FontWeight::SEMIBOLD).child(p.name.clone()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .text_size(l.px(Metric::TypeCaption))
                    .text_color(l.text3())
                    .child(icon(glyph::LOCK, 11.))
                    .child(div().truncate().child(note)),
            )
            .child(
                crate::calendar::icon_button("pv-close", glyph::CLOSE, &l)
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("att-pv-close")).build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_preview(cx);
                    })),
            );
        let body: AnyElement = match &p.state {
            PvState::Loading => centered(div().text_color(l.text2()).child(tr!("att-pv-loading")), &l),
            PvState::Failed(e) => centered(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .max_w(px(520.))
                    .text_color(l.text2())
                    .child(icon(glyph::WARNING, 16.).text_color(l.warning()))
                    .child(tr!("att-pv-failed", error = e.as_str())),
                &l,
            ),
            PvState::Ready(Shown::Image { image, source, format }) => {
                let bar = toolbar(&l)
                    .child(div().px(px(6.)).text_color(l.text2()).child(tr!(
                        "att-pv-image-info",
                        w = source.0,
                        h = source.1,
                        format = format.as_str()
                    )))
                    .child(div().flex_1())
                    .children(self.file_buttons(p, &l, cx));
                // В своём размере, не шире холста; выше — прокрутка.
                let size = image.size(0);
                let canvas = pv_canvas(&p.scroll, "pv-image", &l)
                    .gap(px(10.))
                    .child(picture(image, px(size.width.0 as f32)).justify_center())
                    .child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(format!("{} · {}", p.name, size_text(p.row.size))));
                div().flex_1().min_h_0().flex().flex_col().child(bar).child(canvas).into_any_element()
            }
            PvState::Ready(Shown::Pdf { image, page, pages, title }) => {
                let pager = |id: &'static str, g: char, delta: i64, text: String, on: bool| {
                    crate::calendar::icon_button(id, g, &l)
                        .when(!on, |d| d.opacity(0.4))
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, _, cx| this.pdf_page(delta, cx)))
                };
                let bar = toolbar(&l)
                    .child(pager("pv-prev", glyph::PREV, -1, tr!("att-pv-prev-page"), *page > 0))
                    .child(div().min_w(px(56.)).flex().justify_center().text_color(if p.busy { l.text3() } else { l.text() }).child(tr!(
                        "att-pv-page",
                        page = page + 1,
                        pages = *pages
                    )))
                    .child(pager("pv-next", glyph::NEXT, 1, tr!("att-pv-next-page"), page + 1 < *pages))
                    .child(div().w(px(1.)).h(px(16.)).mx(px(6.)).bg(l.border()))
                    .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).child(title.clone().unwrap_or_default()))
                    .children(self.file_buttons(p, &l, cx));
                // Страница по ширине холста, вниз — прокрутка (макет); листание — кнопками. Пикселей — по ширине
                // холста на экране: заметно другая ширина (первый показ, ресайз) — страница заново.
                let width = p.scroll.bounds().size.width - px(2. * CANVAS_PAD);
                let want = (f32::from(width) * window.scale_factor()).round().max(0.) as u32;
                if want > 0 && !p.busy && needs_refit(p.fit_w, want) {
                    cx.defer_in(window, move |this, _, cx| this.refit_pdf(want, cx));
                }
                let canvas = pv_canvas(&p.scroll, "pv-pdf", &l).child(picture(image, relative(1.)));
                div().flex_1().min_h_0().flex().flex_col().child(bar).child(canvas).into_any_element()
            }
            PvState::Ready(Shown::Archive(list)) => archive(list, &l),
            PvState::Ready(Shown::Text { text, truncated }) => {
                let mut col = div()
                    .id("pv-text")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .font_family("Consolas")
                    .text_size(px(13.))
                    .line_height(px(19.));
                if *truncated {
                    col = col.child(
                        div()
                            .text_size(l.px(Metric::TypeCaption))
                            .text_color(l.text2())
                            .child(tr!("att-pv-text-cut", size = size_text(postman_worker::preview::MAX_TEXT as u64))),
                    );
                }
                col.child(text.clone()).into_any_element()
            }
            PvState::Ready(Shown::Sections { layout, sections, truncated, at, .. }) => {
                self.sections_view(p, *layout, sections, *truncated, *at, &l, cx)
            }
            PvState::Ready(Shown::Letter(m)) => {
                let bar = toolbar(&l).child(div().flex_1()).children(self.file_buttons(p, &l, cx));
                let key = format!("pv-letter-{}", p.row.blob_id);
                div().flex_1().min_h_0().flex().flex_col().child(bar).child(self.letter_view(m, &key, &l, cx)).into_any_element()
            }
            PvState::Ready(Shown::Office(scan)) => self.check_card(p, Some(scan), &l, cx),
            PvState::Ready(Shown::Card) => self.check_card(p, None, &l, cx),
        };
        div()
            .flex_1()
            .min_h(px(360.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .border_1()
            .border_color(l.stroke())
            .overflow_hidden()
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// Документ разделами (S8-23): книга — вкладки листов и сетка; презентация — листание и слайд; прочее — подряд.
    #[allow(clippy::too_many_arguments)]
    fn sections_view(
        &self,
        p: &Pv,
        layout: crate::sections::Layout,
        sections: &[postman_worker::Section],
        truncated: bool,
        at: usize,
        l: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::sections::{self as sec, Layout};
        let key = format!("pv-doc-{}-{at}", p.row.blob_id);
        let cut = |d: Stateful<Div>| d.child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("att-pv-doc-cut")));
        let col = self.column.get();
        match layout {
            Layout::Book => {
                let tabs = sections.len() > 1 || sections.first().is_some_and(|s| !matches!(s.title, postman_worker::SectionTitle::None));
                let mut bar = toolbar(l);
                if tabs {
                    bar = bar.children(sections.iter().enumerate().map(|(i, s)| {
                        let on = i == at;
                        let hint = sec::title(&s.title);
                        div()
                            .id(("pv-sheet", i))
                            .h_full()
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .when(on, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(l.accent_text()).border_b_2().border_color(l.accent()))
                            .when(!on, |d| d.text_color(l.text2()).hover(|d| d.text_color(l.text())))
                            .child(sec::tab_name(s, i))
                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| this.go_section(i, cx)))
                    }));
                }
                let bar = bar.child(div().flex_1()).children(self.file_buttons(p, l, cx));
                let Some(s) = sections.get(at) else { return div().into_any_element() };
                let table = s.blocks.iter().find_map(|b| if let postman_ir::Block::Table(rows) = b { Some(rows) } else { None });
                let body = match table {
                    Some(rows) => sec::grid(rows, &key, l),
                    None => div()
                        .id("pv-doc")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p(px(14.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .children(crate::ir_view::blocks(&s.blocks, &key, &crate::ir_view::Images::new(), col, cx))
                        .into_any_element(),
                };
                // Показано начало (S8-23): строк — сколько дал воркер; «Сохранить…» — весь файл.
                let row = p.row.clone();
                let foot = truncated.then(|| {
                    div()
                        .h(px(32.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(12.))
                        .border_t_1()
                        .border_color(l.border())
                        .bg(l.fill())
                        .text_size(l.px(Metric::TypeCaption))
                        .child(icon(glyph::INFO, 12.).text_color(l.text2()))
                        .child(div().flex_1().text_color(l.text2()).child(tr!("att-pv-cut-rows-shown", shown = sec::rows_of(s) as i64)))
                        .child(
                            div()
                                .id("pv-cut-save")
                                .cursor_pointer()
                                .text_color(l.accent_text())
                                .child(tr!("att-pv-save"))
                                .on_click(cx.listener(move |this, _, _, cx| this.file_action(row.clone(), FileAct::SaveAs, cx))),
                        )
                });
                div().flex_1().min_h_0().flex().flex_col().child(bar).child(body).children(foot).into_any_element()
            }
            Layout::Slides => {
                let n = sections.len();
                let pager = |id: &'static str, g: char, delta: isize, text: String, on: bool| {
                    crate::calendar::icon_button(id, g, l)
                        .when(!on, |d| d.opacity(0.4))
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.step_slide(delta, cx);
                        }))
                };
                let caption = sections.get(at).map(sec::slide_caption).unwrap_or_default();
                let bar = toolbar(l)
                    .child(pager("pv-prev-slide", glyph::PREV, -1, tr!("att-pv-prev-slide"), at > 0))
                    .child(div().min_w(px(56.)).flex().justify_center().child(tr!("att-pv-page", page = (at + 1) as i64, pages = n as i64)))
                    .child(pager("pv-next-slide", glyph::NEXT, 1, tr!("att-pv-next-slide"), at + 1 < n))
                    .child(div().w(px(1.)).h(px(16.)).mx(px(6.)).bg(l.border()))
                    .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).child(caption))
                    .children(self.file_buttons(p, l, cx));
                let Some(s) = sections.get(at) else { return div().into_any_element() };
                let (body, notes) = sec::split_notes(&s.blocks);
                let notes = notes.map(|n| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .pt(px(8.))
                        .child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("att-pv-notes")))
                        .children(crate::ir_view::blocks(n, &format!("{key}-notes"), &crate::ir_view::Images::new(), col, cx))
                });
                let canvas = div()
                    .id("pv-slide")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(20.))
                    .py(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .children(crate::ir_view::blocks(body, &key, &crate::ir_view::Images::new(), col, cx))
                    .children(notes)
                    .when(truncated, cut);
                div().flex_1().min_h_0().flex().flex_col().child(bar).child(canvas).into_any_element()
            }
            Layout::Doc => {
                // Тем же видом, что тело письма (`ir_view::blocks`), с прокруткой; картинок у просмотра нет. Разделы
                // с заголовком (модуль, книга без вкладок) — подписью над своими блоками.
                let mut col_el =
                    div().id("pv-doc").flex_1().min_h_0().overflow_y_scroll().p(px(14.)).flex().flex_col().gap(px(8.)).when(truncated, cut);
                for (i, s) in sections.iter().enumerate() {
                    let t = sec::title(&s.title);
                    if !t.is_empty() {
                        col_el = col_el.child(div().pt(px(6.)).text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(t));
                    }
                    col_el = col_el.children(crate::ir_view::blocks(&s.blocks, &format!("{key}-{i}"), &crate::ir_view::Images::new(), col, cx));
                }
                col_el.into_any_element()
            }
        }
    }

    /// «Открыть в приложении» (если можно) и «Сохранить…».
    fn file_buttons(&self, p: &Pv, l: &Look, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if p.risk == Risk::Safe && files::can_open(&p.row.name) {
            let row = p.row.clone();
            out.push(
                tool_button("pv-open", glyph::OPEN, tr!("att-pv-open-app"), l)
                    .on_click(cx.listener(move |this, _, _, cx| this.file_action(row.clone(), FileAct::Open, cx)))
                    .into_any_element(),
            );
        }
        let row = p.row.clone();
        out.push(
            tool_button("pv-save", glyph::SAVE, tr!("att-pv-save"), l)
                .on_click(cx.listener(move |this, _, _, cx| this.file_action(row.clone(), FileAct::SaveAs, cx)))
                .into_any_element(),
        );
        out
    }

    /// Карточка проверки: макросы и исполняемые — «Открытие заблокировано» (красный — безопасность), иначе —
    /// сведения о файле с «Открыть» и «Сохранить».
    fn check_card(&self, p: &Pv, scan: Option<&OfficeScan>, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let ext = p.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let blocked = p.risk != Risk::Safe;
        let (badge, title, sub) = if blocked {
            let sub = if p.risk == Risk::Macros { tr!("att-pv-macros") } else { tr!("att-pv-exe") };
            (glyph::SHIELD, tr!("att-pv-blocked"), sub)
        } else {
            (glyph::INFO, p.name.clone(), size_text(p.row.size))
        };
        let head = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .size(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(if blocked { l.pm(Pm::ThreatBg) } else { l.fill() })
                    .text_color(if blocked { l.pm(Pm::ThreatFg) } else { l.text2() })
                    .child(icon(badge, 16.)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().truncate().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_size(l.px(Metric::TypeCaption)).text_color(if blocked { l.pm(Pm::ThreatFg) } else { l.text2() }).child(sub)),
            );
        let mut card = div()
            .max_w(px(560.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .px(px(18.))
            .py(px(16.))
            .rounded(px(8.))
            .border_1()
            .border_color(l.stroke())
            .bg(l.panel())
            .child(head);
        if blocked {
            let (found, modules) = match (p.risk, scan) {
                (Risk::Macros, Some(s)) if !s.modules.is_empty() => (tr!("att-pv-found-macros", n = s.modules.len()), Some(s.modules.join(", "))),
                (Risk::Macros, Some(s)) if s.macros => (tr!("att-pv-found-project"), None),
                (Risk::Macros, _) => (tr!("att-pv-found-type", ext = ext.as_str()), None),
                _ => (tr!("att-pv-found-exe", ext = ext.as_str()), None),
            };
            let why = if p.risk == Risk::Macros { tr!("att-pv-why-macros") } else { tr!("att-pv-why-exe") };
            let label = |t: String| div().w(px(150.)).flex_none().text_color(l.text2()).child(t);
            let found = div()
                .flex()
                .flex_col()
                .child(found)
                .children(modules.map(|m| div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(m)));
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().flex().gap(px(12.)).child(label(tr!("att-pv-found"))).child(div().flex_1().min_w_0().child(found)))
                    .child(div().flex().gap(px(12.)).child(label(tr!("att-pv-why"))).child(div().flex_1().min_w_0().child(why))),
            );
            let row = p.row.clone();
            // «Сохранить…» файла с макросами — по настройке «Безопасность» (S5-30, по умолчанию можно, решение №7).
            let save = p.risk != Risk::Macros || crate::prefs::get(cx).security.macro_save;
            card = if save {
                card.child(
                    div().flex().gap(px(8.)).child(
                        crate::buttons::primary("pv-save-blocked", l)
                            .child(icon(glyph::SAVE, 14.))
                            .child(tr!("att-pv-save"))
                            .on_click(cx.listener(move |this, _, _, cx| this.file_action(row.clone(), FileAct::SaveAs, cx))),
                    ),
                )
                .child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("att-pv-save-warning")))
            } else {
                card.child(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("att-pv-save-off")))
            };
        } else {
            let text = if scan.is_some() { tr!("att-pv-office-clean") } else { tr!("att-pv-none") };
            // Подходящий модуль выключен (S8-27): «Включить модуль «…»».
            let enable = self.enable_module_link(&p.row.name, &p.row.mime, l, cx);
            card = card
                .child(div().text_color(l.text2()).child(text))
                .child(div().flex().items_center().gap(px(8.)).children(enable).children(self.file_buttons(p, l, cx)));
        }
        div().id("pv-card").flex_1().min_h_0().overflow_y_scroll().bg(l.base()).px(px(16.)).py(px(14.)).child(card).into_any_element()
    }
}

/// Панель кнопок просмотра 36 px.
fn toolbar(l: &Look) -> Div {
    div()
        .h(px(36.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.))
        .px(px(8.))
        .border_b_1()
        .border_color(l.border())
        .text_size(l.px(Metric::TypeCaption))
}

fn tool_button(id: &'static str, g: char, text: String, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.))
        .px(px(8.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
        .child(icon(g, 12.))
        .child(text)
}

/// Холст картинки и страницы PDF: колонка по центру с прокруткой вниз.
fn pv_canvas(scroll: &ScrollHandle, id: &'static str, l: &Look) -> Stateful<Div> {
    div().id(id).track_scroll(scroll).flex_1().min_h_0().overflow_y_scroll().bg(l.base()).p(px(CANVAS_PAD)).flex().flex_col().items_center()
}

/// Кадр шириной родителя, но не шире `max_w`; высота — по пропорции кадра. Высоту задаёт обёртка: у `img` без
/// абсолютной ширины gpui ставит высоту родного кадра, и `Contain` сдвигает картинку вниз (pets, gpui-pitfalls
/// «Картинки»). Обёртка — в flex-строке: в колонке с `stretch` taffy берёт высоту по пропорции от ширины до
/// `max_w`, и под узкой картинкой остаётся пустота. Итог — строка во всю ширину, кадр слева (`justify_center` —
/// по центру).
pub(crate) fn picture(image: &Arc<RenderImage>, max_w: impl Into<Length> + Clone) -> Div {
    let size = image.size(0);
    let ratio = size.width.0.max(1) as f32 / size.height.0.max(1) as f32;
    let frame = div().flex_1().min_w_0().max_w(max_w).aspect_ratio(ratio).child(img(image.clone()).size_full());
    div().flex_none().w_full().flex().child(frame)
}

/// Перерисовать страницу под ширину `want`: отличается от прежней больше чем на восьмую.
fn needs_refit(fit_w: u32, want: u32) -> bool {
    want.abs_diff(fit_w) > fit_w / 8
}

fn centered(inner: Div, l: &Look) -> AnyElement {
    div().flex_1().min_h_0().flex().items_center().justify_center().p(px(16.)).bg(l.base()).child(inner).into_any_element()
}

/// Оглавление архива: имя, размер, дата; опасные файлы — красным со щитком; итог и предупреждения внизу.
fn archive(list: &Listing, l: &Look) -> AnyElement {
    const W_SIZE: f32 = 90.;
    const W_DATE: f32 = 130.;
    let row = |h: f32| div().h(px(h)).flex_none().flex().items_center().gap(px(12.)).px(px(14.)).border_b_1().border_color(l.border());
    let head = row(30.)
        .bg(l.base())
        .text_size(l.px(Metric::TypeCaption))
        .text_color(l.text2())
        .child(div().flex_1().child(tr!("att-pv-col-name")))
        .child(div().w(px(W_SIZE)).flex().justify_end().child(tr!("att-pv-col-size")))
        .child(div().w(px(W_DATE)).child(tr!("att-pv-col-date")));
    let rows = list.entries.iter().map(|e| {
        let danger = attach::risk(&e.name) != Risk::Safe;
        let name = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .when(danger, |d| d.text_color(l.pm(Pm::ThreatFg)))
            .child(icon(if danger { glyph::SHIELD } else { glyph::DOCUMENT }, 12.))
            .child(div().truncate().child(e.name.clone()))
            .when(e.encrypted, |d| d.child(div().flex_none().text_color(l.text3()).child(tr!("att-pv-locked"))));
        row(32.)
            .child(name)
            .child(div().w(px(W_SIZE)).flex().justify_end().text_color(l.text2()).child(size_text(e.size)))
            .child(div().w(px(W_DATE)).text_color(l.text2()).child(e.modified.clone().unwrap_or_default()))
    });
    let danger = attach::dangerous(list.entries.iter().map(|e| e.name.as_str()));
    let more = list.files.saturating_sub(list.entries.len() as u64);
    let mut foot = div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(14.))
        .py(px(8.))
        .text_size(l.px(Metric::TypeCaption))
        .text_color(l.text2())
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(div().flex_1().child(tr!("att-pv-archive-total", n = list.files, size = size_text(list.size))))
                .children((more > 0).then(|| div().child(tr!("att-pv-archive-more", n = more)))),
        )
        .child(if danger == 0 {
            div().child(tr!("att-pv-archive-safe"))
        } else {
            div().text_color(l.pm(Pm::ThreatFg)).child(tr!("att-pv-archive-danger", n = danger))
        });
    if list.encrypted {
        foot = foot.child(div().child(tr!("att-pv-archive-encrypted")));
    }
    if list.bomb {
        foot = foot.child(div().text_color(l.pm(Pm::ThreatFg)).child(tr!("att-pv-archive-bomb")));
    }
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(head)
        .child(div().id("pv-archive").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().children(rows))
        .child(foot)
        .into_any_element()
}
