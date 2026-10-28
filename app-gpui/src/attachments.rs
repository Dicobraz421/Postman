//! Вложения в области чтения (S5-07, макет «Просмотр вложений»): лента карточек 40 px под шапкой, действия
//! «Просмотр» / «Открыть» / «Сохранить как» по наведению, «Сохранить все…», «+N» — развернуть.
//!
//! Что можно делать с файлом — `postman_core::attach` (имя) и проверка содержимого в воркере (макросы в OLE и
//! OOXML): документ с макросами — красный щиток и полоса, «Открыть» заменено на «Сохранить»; исполняемые — только
//! сохранить. Файлы на диске — `postman_core::files` (безопасное имя, метка «из Интернета»).
//!
//! Перетаскивание в другое приложение (T1-18): нажать на карточке и повести за порог `SM_CXDRAG`/`SM_CYDRAG` —
//! OLE drag source (`sys::dragdrop`). Байты к этому моменту уже в памяти (кэш читается при нажатии, в фоне); нет их —
//! загрузка, курсор «нельзя», по готовности — тост. Файл с макросами и исполняемый не перетаскивается, как и не
//! открывается.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_cache::AttachmentRow;
use postman_core::attach::{self as classify, Risk, Viewer};
use postman_core::files;
use postman_worker::OfficeScan;

use crate::Mailer;
use crate::chrome::icon;
use crate::reading::{Reading, ReadingPane};
use crate::roles::{Look, Pm};

/// Карточек в одном ряду ленты (макет: сетка в 4 колонки).
pub(crate) const PER_ROW: usize = 4;
/// Высота карточки.
const CARD_H: f32 = 40.;

/// Значки Segoe Fluent Icons ленты и просмотра.
pub(crate) mod glyph {
    pub(crate) const SHIELD: char = '\u{EA18}';
    pub(crate) const SAVE: char = '\u{E74E}';
    pub(crate) const OPEN: char = '\u{E8A7}';
    pub(crate) const PREVIEW: char = '\u{E7B3}';
    pub(crate) const ATTACH: char = '\u{E723}';
    pub(crate) const CLOSE: char = '\u{E711}';
    pub(crate) const PREV: char = '\u{E76B}';
    pub(crate) const NEXT: char = '\u{E76C}';
    pub(crate) const INFO: char = '\u{E946}';
    pub(crate) const LOCK: char = '\u{E72E}';
    pub(crate) const WARNING: char = '\u{E7BA}';
    pub(crate) const DOCUMENT: char = '\u{E8A5}';
}

/// Действие с вложением; ждёт байты, если их ещё нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileAct {
    Open,
    SaveAs,
}

/// Вид файла — для плашки карточки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Image,
    Pdf,
    Document,
    Sheet,
    Slides,
    Archive,
    Calendar,
    Mail,
    Text,
    /// Исполняемое и «активное» содержимое: открыть нельзя, только сохранить.
    Blocked,
    Other,
}

/// Строка ленты вложений.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub(crate) row: AttachmentRow,
    /// Имя без путей, управляющих и bidi-символов: подмена `счёт<U+202E>fdp.exe` видна как есть.
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// Опасность с учётом проверки содержимого в воркере.
    pub(crate) risk: Risk,
    pub(crate) viewer: Viewer,
    /// «Открыть» системным приложением: обычный файл без макросов.
    pub(crate) can_open: bool,
}

/// Вложения для ленты под шапкой: картинки, показанные в тексте (`cid:`), и приглашение, показанное карточкой, —
/// не дублируются (как в Outlook). `scans` — проверки Office в воркере по `blob_id`.
pub(crate) fn listed(all: &[AttachmentRow], shown_cids: &HashSet<String>, invite_card: bool, scans: &HashMap<String, OfficeScan>) -> Vec<Item> {
    all.iter()
        .filter(|a| a.cid.as_ref().is_none_or(|c| !shown_cids.contains(c)))
        .filter(|a| !(invite_card && is_invite(a)))
        .map(|a| {
            let risk = classify::risk_with(&a.name, scans.get(&a.blob_id).map(|s| s.macros));
            Item {
                row: a.clone(),
                name: files::safe_name(&a.name),
                kind: kind(&a.name, &a.mime),
                risk,
                viewer: classify::viewer(&a.name, &a.mime),
                can_open: risk == Risk::Safe && files::can_open(&a.name),
            }
        })
        .collect()
}

/// Перетаскивается только безопасный файл: с макросами (красный щиток) и исполняемые — нет, как и не открываются.
pub(crate) fn draggable(item: &Item) -> bool {
    item.risk == Risk::Safe
}

/// Кнопка нажата на карточке вложения: может начаться перетаскивание (T1-18).
pub(crate) struct Drag {
    row: AttachmentRow,
    /// Где нажата кнопка (точки окна).
    start: Point<Pixels>,
    /// Байты (хранилище, демо, загрузка); нет — перетаскивание не начинается.
    data: Option<Arc<Vec<u8>>>,
    /// Порог пройден без байтов: идёт загрузка, по готовности — тост.
    pub(crate) waiting: bool,
    /// Идёт перетаскивание (модальный цикл OLE).
    running: bool,
}

impl Drag {
    pub(crate) fn blob(&self) -> &str {
        &self.row.blob_id
    }
}

/// Вложения ленты открытого письма.
pub(crate) fn items(r: &Reading) -> Vec<Item> {
    let Some(body) = r.body.as_ref() else { return Vec::new() };
    let shown: HashSet<String> = r.doc.as_ref().map(|d| crate::ir_view::cids(d).into_iter().collect()).unwrap_or_default();
    // Приглашение или поручение (S6-10), показанное карточкой.
    let card = r.invite.as_ref().is_some_and(|c| c.invitation().is_some()) || r.task_mail.is_some();
    // Подпись S/MIME (S5-12): `smime.p7s` — значком в шапке, не вложением; `smime.p7m` с подписью внутри — телом.
    // Шифрованное (Ф2): `smime.p7m` — не вложение, а само письмо (до расшифровки — строка, после — его вложения).
    let opaque = r
        .smime
        .as_ref()
        .is_some_and(|s| s.lock.is_some() || matches!(&s.seal, crate::smime::Seal::Done(v) if v.report.kind == postman_smime::Kind::Opaque));
    let all: Vec<AttachmentRow> = body.attachments.iter().filter(|a| !crate::smime::hidden(a, opaque)).cloned().collect();
    listed(&all, &shown, card, &r.scans)
}

/// Вложение — приглашение на встречу (`text/calendar`, `.ics`).
pub(crate) fn is_invite(a: &AttachmentRow) -> bool {
    let mime = a.mime.to_ascii_lowercase();
    mime.starts_with("text/calendar") || mime == "application/ics" || a.name.to_ascii_lowercase().ends_with(".ics")
}

pub(crate) fn kind(name: &str, mime: &str) -> Kind {
    if !files::can_open(name) && name.contains('.') {
        return Kind::Blocked;
    }
    let ext = extension(name);
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" => Kind::Image,
        "pdf" => Kind::Pdf,
        "doc" | "docx" | "docm" | "dot" | "dotx" | "dotm" | "odt" | "rtf" => Kind::Document,
        "xls" | "xlsx" | "xlsm" | "xlsb" | "xlt" | "xltx" | "xltm" | "xlam" | "ods" | "csv" => Kind::Sheet,
        "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "ppsm" | "pot" | "potx" | "potm" | "odp" => Kind::Slides,
        "zip" | "7z" | "rar" | "gz" | "tar" => Kind::Archive,
        "ics" => Kind::Calendar,
        "eml" | "msg" => Kind::Mail,
        "txt" | "log" | "md" => Kind::Text,
        _ if mime.starts_with("image/") => Kind::Image,
        _ if mime.starts_with("text/calendar") => Kind::Calendar,
        _ if mime.starts_with("text/") => Kind::Text,
        _ => Kind::Other,
    }
}

fn extension(name: &str) -> String {
    let safe = files::safe_name(name);
    safe.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Подпись плашки: расширение заглавными, до 4 знаков («PDF», «XLSM»); без расширения — по виду.
pub(crate) fn tile_label(item: &Item) -> String {
    let ext = extension(&item.name);
    if !ext.is_empty() && ext.chars().count() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return ext.to_ascii_uppercase();
    }
    match item.kind {
        Kind::Image => "IMG",
        Kind::Text => "TXT",
        _ => "FILE",
    }
    .to_string()
}

/// Размер для показа: (единица для `.ftl`, число). Байты и КБ — целые, МБ — с одним знаком.
pub(crate) fn size_parts(n: u64) -> (&'static str, f64) {
    const K: f64 = 1024.0;
    match n {
        n if n < 1024 => ("b", n as f64),
        n if n < 1024 * 1024 => ("kb", (n as f64 / K).round()),
        n => ("mb", (n as f64 / K / K * 10.0).round() / 10.0),
    }
}

pub(crate) fn size_text(n: u64) -> String {
    let (unit, n) = size_parts(n);
    tr!("reading-att-size", unit = unit, n = n)
}

/// Сколько карточек показать в свёрнутой ленте: всё, что влезает в ряд; не влезает — ряд без последней и «+N».
pub(crate) fn visible(total: usize, expanded: bool) -> (usize, usize) {
    if expanded || total <= PER_ROW { (total, 0) } else { (PER_ROW - 1, total - (PER_ROW - 1)) }
}

/// Команды вложений — на корне экрана почты (`Mailer::render`). ← / → без открытого просмотра — беседе в списке
/// «как Outlook» (свернуть, развернуть; S5-05б), не нужны и ей — действие уходит дальше (`propagate`).
pub(crate) fn handlers(root: Div, cx: &mut Context<Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &attach::Preview, _, cx| this.reading.update(cx, |r, cx| r.toggle_preview(None, cx))))
        .on_action(cx.listener(|this, _: &crate::actions::mail::Signature, _, cx| this.reading.update(cx, |r, cx| r.toggle_smime(cx))))
        .on_action(cx.listener(|this, _: &attach::SaveAll, _, cx| this.reading.update(cx, |r, cx| r.save_all(cx))))
        .on_action(cx.listener(|this, _: &attach::Next, _, cx| {
            if !this.reading.update(cx, |r, cx| r.step_preview(1, cx)) && !this.thread_arrow(crate::outline::Fold::Expand, cx) {
                cx.propagate();
            }
        }))
        .on_action(cx.listener(|this, _: &attach::Prev, _, cx| {
            if !this.reading.update(cx, |r, cx| r.step_preview(-1, cx)) && !this.thread_arrow(crate::outline::Fold::Collapse, cx) {
                cx.propagate();
            }
        }))
}

/// Тихая кнопка-значок 24 px (действия карточки).
fn act_button(id: impl Into<ElementId>, g: char, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(l.px(Metric::RadiusControl))
        .text_color(l.text2())
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()).text_color(l.text()))
        .child(icon(g, 12.))
}

fn tip(text: String) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |window: &mut Window, cx: &mut App| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
}

impl ReadingPane {
    /// Лента под шапкой: «Вложения · N · размер» и «Сохранить все…», ниже — карточки по 4 в ряд.
    pub(crate) fn ribbon(&self, r: &Reading, cx: &mut Context<Self>) -> Option<AnyElement> {
        let items = items(r);
        if items.is_empty() {
            return None;
        }
        let l = crate::roles::look(cx);
        let total: u64 = items.iter().map(|i| i.row.size).sum();
        let picked = r.preview.as_ref().map(|p| p.row.blob_id.clone());
        let saving = self.save_all.is_some();
        let head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .child(icon(glyph::ATTACH, 12.))
            .child(div().flex_1().child(tr!("att-ribbon", n = items.len(), size = size_text(total))))
            .child(
                div()
                    .id("att-save-all")
                    .h(px(22.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .text_color(if saving { l.text3() } else { l.accent_text() })
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(if saving { tr!("att-loading") } else { tr!("att-save-all") })
                    .on_click(cx.listener(|this, _, _, cx| this.save_all(cx))),
            );
        let (shown, more) = visible(items.len(), r.expanded);
        let mut cards: Vec<AnyElement> =
            items.iter().take(shown).enumerate().map(|(n, item)| self.card(n, item, picked.as_deref(), &l, cx)).collect();
        if more > 0 {
            cards.push(
                div()
                    .id("att-more")
                    .h(px(CARD_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(l.stroke())
                    .bg(l.panel())
                    .text_color(l.text2())
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(tr!("att-more", n = more))
                    .on_click(cx.listener(|this, _, _, cx| this.set_expanded(true, cx)))
                    .into_any_element(),
            );
        } else if r.expanded && items.len() > PER_ROW {
            cards.push(
                div()
                    .id("att-less")
                    .h(px(CARD_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_color(l.accent_text())
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(tr!("att-less"))
                    .on_click(cx.listener(|this, _, _, cx| this.set_expanded(false, cx)))
                    .into_any_element(),
            );
        }
        let grid = div().grid().grid_cols(PER_ROW as u16).gap(px(8.)).children(cards);
        Some(
            div()
                .id("att-ribbon")
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(head)
                .child(grid)
                // Перетаскивание (T1-18): порог — по движению над лентой, уход с ленты с нажатой кнопкой — тоже.
                .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| this.drag_move(ev.position, ev.pressed_button, window, cx)))
                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                    if !*hovered && this.drag.as_ref().is_some_and(|d| !d.running && !d.waiting) {
                        this.drag_go(cx);
                    }
                    let _ = window;
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.drag_release(cx)))
                .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| this.drag_release(cx)))
                .into_any_element(),
        )
    }

    /// Карточка 40 px: плашка типа, имя и размер, справа — щиток опасности; по наведению — действия.
    fn card(&self, n: usize, item: &Item, picked: Option<&str>, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let on = picked == Some(item.row.blob_id.as_str());
        let danger = item.risk != Risk::Safe;
        let pending = self.pending_files.iter().any(|(p, _)| p.blob_id == item.row.blob_id);
        let group: SharedString = format!("att-card-{n}").into();
        let tile = div()
            .w(px(28.))
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.))
            .bg(if danger { l.pm(Pm::ThreatBg) } else { l.fill() })
            .text_color(if danger { l.pm(Pm::ThreatFg) } else { l.text2() })
            .text_size(px(9.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(tile_label(item));
        let size = if pending { tr!("att-loading") } else { size_text(item.row.size) };
        let text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(div().truncate().text_size(px(13.)).line_height(px(17.)).text_color(l.text()).child(item.name.clone()))
            .child(div().text_size(px(11.)).line_height(px(14.)).text_color(l.text2()).child(size));
        let status = match item.risk {
            Risk::Macros => Some((tr!("att-status-macros"), glyph::SHIELD)),
            Risk::Blocked => Some((tr!("att-status-blocked"), glyph::SHIELD)),
            Risk::Safe => None,
        };
        let row = item.row.clone();
        let blob = item.row.blob_id.clone();
        let can_drag = draggable(item);
        let drag_row = item.row.clone();
        let waiting = self.drag.as_ref().is_some_and(|d| d.waiting && d.row.blob_id == item.row.blob_id);
        let mut actions = div()
            .absolute()
            .top(px(7.))
            .right(px(4.))
            .flex()
            .gap(px(2.))
            .bg(if on { l.picked() } else { l.panel() })
            .opacity(0.)
            .group_hover(group.clone(), |s| s.opacity(1.));
        if item.viewer != Viewer::None || danger {
            let b = blob.clone();
            actions = actions.child(act_button(("att-pv", n), glyph::PREVIEW, l).tooltip(tip(tr!("att-preview"))).on_click(cx.listener(
                move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_preview(Some(b.clone()), cx);
                },
            )));
        }
        if item.can_open {
            let r = row.clone();
            actions = actions.child(act_button(("att-open", n), glyph::OPEN, l).tooltip(tip(tr!("att-open"))).on_click(cx.listener(
                move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.file_action(r.clone(), FileAct::Open, cx);
                },
            )));
        }
        actions =
            actions.child(act_button(("att-save", n), glyph::SAVE, l).tooltip(tip(tr!("att-save"))).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.file_action(row.clone(), FileAct::SaveAs, cx);
            })));
        let mut card = div()
            .id(("att-card", n))
            .group(group)
            .relative()
            .h(px(CARD_H))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(if danger { 11. } else { 8. }))
            .pr(px(8.))
            .rounded(px(6.))
            .overflow_hidden()
            .border_1()
            .border_color(if on { l.accent_text() } else { l.stroke() })
            .bg(if on { l.picked() } else { l.panel() })
            .cursor_pointer()
            .tooltip(tip(format!("{} · {}", item.name, size_text(item.row.size))))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_preview(Some(blob.clone()), cx)))
            .when(can_drag, |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| this.drag_press(drag_row.clone(), ev.position, cx)),
                )
            })
            .when(waiting, |d| d.cursor(CursorStyle::OperationNotAllowed))
            .child(tile)
            .child(text);
        if let Some((text, g)) = status {
            card = card.child(div().id(("att-risk", n)).flex_none().text_color(l.pm(Pm::ThreatFg)).tooltip(tip(text)).child(icon(g, 14.)));
        }
        if danger {
            // Тонкая красная полоса слева (DESIGN «Вложения»).
            card = card.child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(l.pm(Pm::ThreatDot)));
        }
        card.child(actions).into_any_element()
    }
}

impl ReadingPane {
    /// Кнопка нажата на карточке: запомнить и взять байты без сети — демо сразу, живой ящик — из хранилища в фоне.
    fn drag_press(&mut self, row: AttachmentRow, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.drag.as_ref().is_some_and(|d| d.running || d.waiting) {
            return;
        }
        let blob = row.blob_id.clone();
        let demo = (!self.live).then(|| postman_demo::blob(&blob)).flatten().map(Arc::new);
        self.drag = Some(Drag { row, start: at, data: demo, waiting: false, running: false });
        if !self.live {
            return;
        }
        let cache = self.cache.clone();
        let key = blob.clone();
        let task = cx.background_spawn(async move { cache.blob(&key).ok().flatten() });
        cx.spawn(async move |this, cx| {
            let data = task.await;
            this.update(cx, |this, _| {
                if let Some(d) = this.drag.as_mut().filter(|d| d.row.blob_id == blob && d.data.is_none()) {
                    d.data = data.map(Arc::new);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Движение с нажатой кнопкой: за порогом — перетаскивание.
    fn drag_move(&mut self, at: Point<Pixels>, button: Option<MouseButton>, window: &Window, cx: &mut Context<Self>) {
        let Some(d) = &self.drag else { return };
        if d.running || d.waiting {
            return;
        }
        if button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let (tx, ty) = drag_threshold(window.scale_factor());
        let (dx, dy) = (f32::from(at.x - d.start.x).abs(), f32::from(at.y - d.start.y).abs());
        if dx > tx || dy > ty {
            self.drag_go(cx);
        }
    }

    /// Порог пройден: байты есть — копия для `CF_HDROP` в фоне и `DoDragDrop`; нет — загрузка и тост по готовности.
    fn drag_go(&mut self, cx: &mut Context<Self>) {
        let Some(d) = &mut self.drag else { return };
        let Some(data) = d.data.clone() else {
            d.waiting = true;
            let row = d.row.clone();
            self.fetch(&row, cx);
            cx.notify();
            return;
        };
        d.running = true;
        let name = files::safe_name(&d.row.name);
        self.start_drag(name, data, cx);
    }

    #[cfg(windows)]
    fn start_drag(&mut self, name: String, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        use crate::sys::dragdrop;
        let file = dragdrop::DragFile { name: name.clone(), data: data.clone() };
        let copy =
            cx.background_spawn(
                async move { dragdrop::prepare(&dragdrop::temp_root(), &name, &data).map_err(|e| tracing::warn!("drag copy: {e}")).ok() },
            );
        cx.spawn(async move |this, cx| {
            let copy = copy.await;
            // Модальный цикл OLE — вне `update`: приложение свободно, окно рисуется и отвечает.
            let result = dragdrop::drag(&file, copy);
            this.update(cx, |this, cx| {
                this.drag = None;
                if let Err(e) = result {
                    cx.emit(crate::reading::ReadingEvent::Toast(pets_ui::Level::Error, tr!("att-drag-failed", name = file.name.clone(), error = e)));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(not(windows))]
    fn start_drag(&mut self, _: String, _: Arc<Vec<u8>>, _: &mut Context<Self>) {
        self.drag = None;
    }

    /// Кнопка отпущена до порога — перетаскивания не было.
    fn drag_release(&mut self, cx: &mut Context<Self>) {
        if self.drag.as_ref().is_some_and(|d| !d.running && !d.waiting) {
            self.drag = None;
            cx.notify();
        }
    }

    /// Байты пришли: ждали их после порога — тост «можно перетаскивать», иначе — в запас нажатия.
    pub(crate) fn drag_bytes(&mut self, blob_id: &str, data: &Arc<Vec<u8>>, cx: &mut Context<Self>) {
        let Some(d) = self.drag.as_mut().filter(|d| d.row.blob_id == blob_id) else { return };
        if d.waiting {
            let name = files::safe_name(&d.row.name);
            self.drag = None;
            cx.emit(crate::reading::ReadingEvent::Toast(pets_ui::Level::Info, tr!("att-drag-ready", name = name)));
        } else {
            d.data = Some(data.clone());
        }
    }
}

/// Порог начала перетаскивания (точки окна): системный на Windows, иначе 4.
fn drag_threshold(scale: f32) -> (f32, f32) {
    #[cfg(windows)]
    return crate::sys::dragdrop::threshold(scale);
    #[cfg(not(windows))]
    {
        let _ = scale;
        (4., 4.)
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "attach", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/attachments.ftl")), ("en", include_str!("../i18n/en/attachments.ftl"))];

/// Вложения открытого письма (S5-07): встроенный просмотр, «Сохранить все», по вложениям в просмотре.
pub(crate) mod attach {
    gpui_kit::actions!(attach, [Preview, SaveAll, Next, Prev]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{LIST, MAIL, cmd};
    &[
        // Не Outlook (там просмотр — щелчком по вложению): просмотр и «Сохранить все» — кнопками ленты; в открытом
        // просмотре ← / → — по вложениям письма, Esc — закрыть (как поиск: `search.clear`).
        cmd("attach.preview", &[], MAIL),
        cmd("attach.save_all", &[], MAIL),
        cmd("attach.next", &["right"], LIST),
        cmd("attach.prev", &["left"], LIST),
    ]
};

/// Сцены `--screens` вложений (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_cache::BodyRow;

    use crate::Mailer;
    use crate::screens::{self, Scene};

    pub(crate) const SCENES: &[Scene] = &[
        // Письмо с вложениями и картинкой `cid:` в тексте (S3-03): лента 40 px с «+N» (S5-07).
        Scene::new("attachments", letter, letter_ready),
        // Встроенный просмотр вложений того же письма (S5-07): фото, страница PDF, оглавление архива, книга с
        // макросами (карточка «Открытие заблокировано»).
        Scene::new("preview-image", preview, preview_ready).continues(),
        Scene::new("preview-pdf", preview, preview_ready).continues(),
        // Та же страница PDF, прокрученная до низа (S5-07б): обрезанный прокруткой кадр не сдвинут.
        Scene::new("preview-pdf-end", pdf_end, pdf_end_ready).continues(),
        Scene::new("preview-zip", preview, preview_ready).continues(),
        Scene::new("attach-macro", preview, preview_ready).continues(),
        // Письмо с большой картинкой в тексте (S5-07б): кадр 1600 px, уменьшен до ширины области чтения, целиком.
        Scene::new("letter-image", letter, letter_ready),
    ];

    /// Письмо сцены: с большой картинкой в тексте или с вложениями для просмотра и картинкой `cid:`.
    fn wanted(s: &Scene) -> fn(&BodyRow) -> bool {
        if s.name == "letter-image" { |b| screens::has(b, "chart") } else { screens::outlook_letter }
    }

    fn letter(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        screens::select_demo(m, wanted(s), cx)
    }

    fn letter_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        let target = screens::demo_id(m.list.read(cx), wanted(s));
        target.is_some() && screens::reading(m, cx).is_some_and(|r| Some(r.row.id.as_str()) == target && r.doc.is_some() && !r.images.is_empty())
    }

    /// Вложение демо-письма для сцены просмотра (`postman_demo::blob`).
    fn blob(s: &Scene) -> &'static str {
        match s.name {
            "preview-image" => "photo",
            "preview-pdf" => "pdf",
            "preview-zip" => "zip",
            _ => "xlsm",
        }
    }

    fn preview(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Как щелчок по карточке ленты.
        let blob = blob(s).to_string();
        m.reading.update(cx, |r, cx| r.toggle_preview(Some(blob), cx));
        Ok(())
    }

    fn preview_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        // Просмотр разобран воркером; у книги с макросами — и проверка для щитка в ленте.
        let blob = blob(s);
        screens::reading(m, cx).is_some_and(|r| {
            let ready = r.preview.as_ref().is_some_and(|p| p.row.blob_id == blob && p.ready());
            ready && (s.name != "attach-macro" || r.scans.contains_key(blob))
        })
    }

    fn pdf_end(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.reading.update(cx, |r, cx| {
            // Как прокрутка колесом до конца: смещение за край gpui прижимает к низу при раскладке.
            if let Some(p) = r.reading.as_ref().and_then(|r| r.preview.as_ref()) {
                p.scroll.set_offset(point(px(0.), px(-100_000.)));
            }
            cx.notify();
        });
        Ok(())
    }

    fn pdf_end_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::reading(m, cx).and_then(|r| r.preview.as_ref()).is_some_and(|p| {
            // Прокручено до низа и страница не перерисовывается.
            let (offset, max) = (p.scroll.offset().y, p.scroll.max_offset().y);
            p.ready() && max > px(0.) && (offset + max).abs() < px(1.)
        })
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use std::collections::{HashMap, HashSet};

    use postman_cache::AttachmentRow;
    use postman_core::attach::{Risk, Viewer};
    use postman_worker::OfficeScan;

    use super::{Item, Kind, is_invite, kind, listed, size_parts, tile_label, visible};

    fn att(name: &str, mime: &str, cid: Option<&str>) -> AttachmentRow {
        AttachmentRow { blob_id: name.into(), name: name.into(), mime: mime.into(), size: 10, cid: cid.map(Into::into), inline: cid.is_some() }
    }

    #[test]
    fn inline_images_and_invite_card_are_not_listed_twice() {
        let all = [
            att("logo.png", "image/png", Some("logo@corp")),
            att("other.png", "image/png", Some("unused@corp")),
            att("Итоги Q3.xlsx", "application/vnd.ms-excel", None),
            att("invite.ics", "text/calendar", None),
        ];
        let shown: HashSet<String> = ["logo@corp".to_string()].into();
        let none = HashMap::new();
        let names = |v: Vec<Item>| v.into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert_eq!(names(listed(&all, &shown, true, &none)), ["other.png", "Итоги Q3.xlsx"]);
        // Карточка не разобралась — приглашение остаётся файлом.
        assert_eq!(names(listed(&all, &shown, false, &none)), ["other.png", "Итоги Q3.xlsx", "invite.ics"]);
        assert_eq!(listed(&all, &HashSet::new(), true, &none).len(), 3);
    }

    #[test]
    fn dangerous_names_are_shown_safe_and_cannot_be_opened() {
        let all = [att("счёт\u{202e}fdp.exe", "application/octet-stream", None), att("invoice.pdf.exe", "", None)];
        let items = listed(&all, &HashSet::new(), false, &HashMap::new());
        assert_eq!(items[0].name, "счётfdp.exe");
        assert!(items.iter().all(|i| !i.can_open && i.kind == Kind::Blocked && i.risk == Risk::Blocked));
    }

    /// Макросы: по имени (`.xlsm`) и по проверке воркера (`.xlsx` с `vbaProject.bin`) — открыть нельзя.
    #[test]
    fn macro_documents_cannot_be_opened() {
        let all = [att("Расчёт.xlsm", "", None), att("Итоги.xlsx", "", None), att("Отчёт.docx", "", None)];
        let scan = |macros| OfficeScan { macros, ..OfficeScan::default() };
        let scans: HashMap<String, OfficeScan> = [("Итоги.xlsx".into(), scan(true)), ("Отчёт.docx".into(), scan(false))].into();
        let items = listed(&all, &HashSet::new(), false, &scans);
        assert_eq!(items.iter().map(|i| i.risk).collect::<Vec<_>>(), [Risk::Macros, Risk::Macros, Risk::Safe]);
        assert_eq!(items.iter().map(|i| i.can_open).collect::<Vec<_>>(), [false, false, true]);
        assert_eq!(items.iter().map(|i| i.viewer).collect::<Vec<_>>(), [Viewer::Office, Viewer::Office, Viewer::Document]);
        // Перетаскивание (T1-18): у файла с макросами источника нет, как и «Открыть».
        assert_eq!(items.iter().map(super::draggable).collect::<Vec<_>>(), [false, false, true]);
        let exe = listed(&[att("setup.exe", "", None), att("photo.jpg", "image/jpeg", None)], &HashSet::new(), false, &HashMap::new());
        assert_eq!(exe.iter().map(super::draggable).collect::<Vec<_>>(), [false, true], "исполняемый — тоже нет");
    }

    #[test]
    fn kinds_and_tiles() {
        assert_eq!(kind("Итоги Q3.xlsx", ""), Kind::Sheet);
        assert_eq!(kind("Расчёт.xlsm", ""), Kind::Sheet);
        assert_eq!(kind("scan.PDF", ""), Kind::Pdf);
        assert_eq!(kind("photo", "image/jpeg"), Kind::Image);
        assert_eq!(kind("invite.ics", "text/calendar"), Kind::Calendar);
        assert_eq!(kind("page.html", "text/html"), Kind::Blocked);
        assert_eq!(kind("noext", "application/octet-stream"), Kind::Other);
        assert!(is_invite(&att("x", "text/calendar; method=REQUEST", None)));
        assert!(is_invite(&att("Meeting.ICS", "application/octet-stream", None)));
        assert!(!is_invite(&att("a.txt", "text/plain", None)));
        let tile = |name: &str, mime: &str| tile_label(&listed(&[att(name, mime, None)], &HashSet::new(), false, &HashMap::new())[0]);
        assert_eq!(tile("Отчёт.pdf", ""), "PDF");
        assert_eq!(tile("Расчёт.xlsm", ""), "XLSM");
        assert_eq!(tile("photo", "image/png"), "IMG");
        assert_eq!(tile("data.backup", ""), "FILE");
    }

    #[test]
    fn overflow_shows_plus_n() {
        assert_eq!(visible(3, false), (3, 0));
        assert_eq!(visible(4, false), (4, 0));
        assert_eq!(visible(6, false), (3, 3));
        assert_eq!(visible(6, true), (6, 0));
    }

    #[test]
    fn sizes() {
        assert_eq!(size_parts(512), ("b", 512.0));
        assert_eq!(size_parts(18_432), ("kb", 18.0));
        assert_eq!(size_parts(1_572_864), ("mb", 1.5));
    }
}
