//! Ответ на фрагменты (решение владельца `docs/decisions/2026-11-14-messenger-comforts.md`, карточка MSGR-01).
//!
//! - Текст письма и ленты беседы выделяется мышью: абзацы письма (`ir_view::text`) — участники выделения окна
//!   gpui-kit (`TextSelectionLayer` у корня окна; Ctrl+C копирует). Что выделено и из какого письма — [`picked`].
//! - «Ответить на фрагмент» (`mail.reply_fragment`): кнопкой над выделением, пунктом меню правой кнопки, палитрой.
//!   Ответ на эту беседу открыт — реплика добавляется в него; нет — в ленте беседы реплика встаёт в быстрый ответ под
//!   лентой (`quick.rs`, «Ответить всем»), в письме — открывается ответ (`mail.reply`) с ней.
//! - Черновик — сверху свой текст, под ним реплики «цитата → мой ответ» по порядку добавления, подпись — внизу. Порядок
//!   — перетаскиванием за ручку и командами `compose.fragment_up` / `compose.fragment_down`; «×» убирает реплику,
//!   Ctrl+Z (`compose.fragment_restore`) возвращает. Данные реплик — `postman_compose::fragments`, в модели письма
//!   ([`Pairs`]); при отправке — обычный Markdown письма ([`Model::markdown`]).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use gpui_kit::base::{TextSelectionHandle, TextSelectionRegistration, TextSelectionRun};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use pets_ui::theme::{Metric, Role};
use postman_cache::EmailRow;
use postman_compose::Mode;
use postman_compose::fragments::{Fragment, Replies, assemble};

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::compose::{Field, Model, own_end, swap_signature};
use crate::composer::Composer;
use crate::reading::ReadingPane;
use crate::roles::Look;

/// Ручка перетаскивания реплики (Segoe Fluent «Sort», вне Windows — `arrow-down-up`).
const HANDLE: char = '\u{E8CB}';
const UP: char = '\u{E70E}';
/// Высота своего текста над репликами (строки поля).
const HEAD_H: f32 = 64.;

// --- данные реплик в модели письма ---

/// Реплики письма и хвост текста — подпись и полная цитата ответа: пока реплики есть, хвост стоит под ними, а не в
/// поле своего текста.
#[derive(Debug, Default, Clone)]
pub(crate) struct Pairs {
    pub(crate) list: Replies,
    pub(crate) tail: String,
}

impl Hash for Pairs {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.list.items().hash(h);
        self.tail.hash(h);
    }
}

impl Model {
    pub(crate) fn has_fragments(&self) -> bool {
        !self.fragments.list.is_empty()
    }

    /// Текст письма для отправки и черновика: свой текст, реплики, подпись и цитата.
    pub(crate) fn markdown(&self) -> String {
        if !self.has_fragments() && self.fragments.tail.is_empty() {
            return self.body.clone();
        }
        assemble(&self.body, &self.fragments.list, &self.fragments.tail)
    }

    /// Новая реплика в конец. Первая — подпись и цитата уходят из поля своего текста под реплики (`true` — текст поля
    /// изменился).
    pub(crate) fn add_fragment(&mut self, f: Fragment) -> (u64, bool) {
        let split = !self.has_fragments() && self.split_tail();
        (self.fragments.list.add(f), split)
    }

    /// Убрать реплику; последняя — подпись и цитата возвращаются в поле (`true` — текст поля изменился).
    pub(crate) fn remove_fragment(&mut self, id: u64) -> Option<bool> {
        if !self.fragments.list.remove(id) {
            return None;
        }
        Some(!self.has_fragments() && self.join_tail())
    }

    /// Вернуть последнюю убранную реплику: её номер и изменился ли текст поля.
    pub(crate) fn restore_fragment(&mut self) -> Option<(u64, bool)> {
        if !self.fragments.list.can_undo() {
            return None;
        }
        let split = !self.has_fragments() && self.split_tail();
        self.fragments.list.undo().map(|id| (id, split))
    }

    /// Подпись письма с репликами — в хвосте: сменить её там. `false` — реплик нет, подпись в поле.
    pub(crate) fn swap_tail_signature(&mut self, signature: &str) -> bool {
        if !self.has_fragments() {
            return false;
        }
        self.fragments.tail = swap_signature(&self.fragments.tail, &self.signature, signature).trim_start_matches('\n').to_owned();
        self.signature = signature.trim_end().to_owned();
        true
    }

    /// Подпись (последняя в своём тексте) и всё после неё — из поля в хвост. `true` — текст поля изменился.
    fn split_tail(&mut self) -> bool {
        let at = tail_start(&self.body, &self.signature);
        let tail = self.body[at..].trim_matches('\n').to_owned();
        let head = self.body[..at].trim_end_matches('\n').to_owned();
        let changed = !tail.is_empty() || head != self.body;
        self.fragments.tail = tail;
        if changed {
            self.set(Field::Body, head);
        }
        changed
    }

    /// Хвост — обратно в поле под свой текст. `true` — текст поля изменился.
    fn join_tail(&mut self) -> bool {
        let tail = std::mem::take(&mut self.fragments.tail);
        if tail.is_empty() {
            return false;
        }
        let head = self.body.trim_end_matches('\n');
        let body = format!("{head}\n\n{tail}");
        self.set(Field::Body, body);
        true
    }
}

/// Где начинается хвост текста: подпись, последняя в своём тексте, иначе — строка перед цитатой ответа или конец.
pub(crate) fn tail_start(body: &str, signature: &str) -> usize {
    let end = own_end(body);
    let sig = signature.trim_end();
    if !sig.is_empty()
        && let Some(at) = body[..end].rfind(sig)
    {
        return at;
    }
    end
}

/// Ответ, который ждёт тела письма (`actions::respond_now`): реплики, набранный текст быстрого ответа (MSGR-02),
/// упомянутые в нём люди и отправить ли сразу.
#[derive(Default, Debug, Clone, PartialEq)]
pub(crate) struct Prepared {
    pub(crate) letter: String,
    pub(crate) fragments: Vec<Fragment>,
    pub(crate) text: String,
    pub(crate) mentioned: Vec<postman_compose::Address>,
    pub(crate) send: bool,
}

#[derive(Default)]
pub(crate) struct Pending {
    pub(crate) pending: Option<Prepared>,
}

impl Pending {
    /// Ждущее для ответа на письмо `letter` — в черновик `d`: реплики; текст — своим текстом над подписью, а с
    /// репликами — ответом на последнюю; упомянутые — в «Кому». `true` — отправить сразу.
    pub(crate) fn apply(&mut self, letter: &str, d: &mut postman_compose::Draft) -> bool {
        let Some(p) = self.pending.take_if(|p| p.letter == letter) else { return false };
        let mut fragments = p.fragments;
        let text = p.text.trim();
        if !text.is_empty() {
            match fragments.last_mut() {
                Some(f) => text.clone_into(&mut f.answer),
                None => d.markdown = format!("{text}{}", d.markdown),
            }
        }
        d.fragments = fragments;
        for a in p.mentioned {
            if !d.to.iter().chain(&d.cc).any(|x| x.email.eq_ignore_ascii_case(&a.email)) {
                d.to.push(a);
            }
        }
        p.send
    }

    /// Обычный ответ (`mail.reply`): ждущий фрагмент к нему не относится.
    pub(crate) fn clear(&mut self) {
        self.pending = None;
    }
}

// --- выделение текста письма ---

/// Выделенный кусок абзаца письма (последний кадр).
struct Piece {
    handle: TextSelectionHandle,
    letter: SharedString,
    order: u64,
    text: String,
    /// Конец выделения в окне: под ним — кнопка «Ответить на фрагмент».
    end: Point<Pixels>,
}

thread_local! {
    /// Выделенные куски по участнику выделения: их пишет отрисовка абзацев, читает [`picked`].
    static PIECES: RefCell<HashMap<EntityId, Piece>> = RefCell::default();
    /// Выделение, снятое нажатием кнопки «Ответить на фрагмент» (нажатие мыши снимает выделение окна раньше щелчка).
    static STASH: RefCell<Option<Picked>> = const { RefCell::new(None) };
}

/// Что выделено: письмо (первое по порядку чтения), текст его кусков, конец выделения, идёт ли ещё протяжка.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Picked {
    pub(crate) letter: SharedString,
    pub(crate) text: String,
    pub(crate) end: Point<Pixels>,
    pub(crate) selecting: bool,
}

/// Выделение в письмах сейчас.
pub(crate) fn picked(cx: &App) -> Option<Picked> {
    PIECES.with(|p| {
        let mut p = p.borrow_mut();
        p.retain(|_, piece| piece.handle.snapshot(cx).is_some());
        let first = p.values().min_by_key(|x| x.order)?;
        let letter = first.letter.clone();
        let mut mine: Vec<&Piece> = p.values().filter(|x| x.letter == letter).collect();
        mine.sort_by_key(|x| x.order);
        let text = join_pieces(mine.iter().map(|x| x.text.as_str()));
        if text.trim().is_empty() {
            return None;
        }
        let end = mine.last().map(|x| x.end)?;
        let selecting = mine.iter().any(|x| x.handle.snapshot(cx).is_some_and(|s| s.is_selecting()));
        Some(Picked { letter, text, end, selecting })
    })
}

/// Куски абзацев — абзацами; пробелы по краям выделения не в счёт.
fn join_pieces<'a>(pieces: impl Iterator<Item = &'a str>) -> String {
    pieces.map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")
}

/// Запомнить выделение для `mail.reply_fragment` (кнопка, меню, сцена снимков).
pub(crate) fn stash(p: Picked) {
    STASH.with(|s| *s.borrow_mut() = Some(p));
}

fn take_stash() -> Option<Picked> {
    STASH.with(|s| s.borrow_mut().take())
}

/// Текст письма в отрисовке (`ir_view::Hits::selectable`): чей view перерисовать, какое письмо и номер следующего
/// абзаца по порядку чтения.
pub(crate) struct Origin {
    view: EntityId,
    letter: RefCell<SharedString>,
    next: Cell<u64>,
}

impl Origin {
    pub(crate) fn new(view: EntityId) -> Self {
        Self { view, letter: RefCell::default(), next: Cell::new(0) }
    }

    /// Дальше — текст письма `id`; номера абзацев — с `base` (письма ленты — по порядку на экране).
    pub(crate) fn letter(&self, id: &str, base: u64) {
        *self.letter.borrow_mut() = SharedString::from(id.to_owned());
        self.next.set(base);
    }

    fn take(&self) -> (SharedString, u64) {
        let n = self.next.get();
        self.next.set(n + 1);
        (self.letter.borrow().clone(), n)
    }
}

/// Абзац письма `child` (текст `text`, раскладка `layout`) — участник выделения окна.
pub(crate) fn selectable(child: AnyElement, text: String, layout: TextLayout, key: &str, origin: &Origin) -> AnyElement {
    let (letter, order) = origin.take();
    // Ключ — с письмом: у другого письма свои участники, выделение прежнего не переходит на него.
    let id = ElementId::Name(SharedString::from(format!("{letter}/{key}")));
    Selectable { id, child, text: SharedString::from(text), layout, letter, order, view: origin.view }.into_any_element()
}

struct Selectable {
    id: ElementId,
    child: AnyElement,
    text: SharedString,
    layout: TextLayout,
    letter: SharedString,
    order: u64,
    view: EntityId,
}

impl IntoElement for Selectable {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Selectable {
    type RequestLayoutState = TextSelectionHandle;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text = self.text.clone();
        let handle = match id {
            Some(id) => window.with_element_state(id, |kept: Option<TextSelectionHandle>, _| {
                let handle = kept.unwrap_or_else(|| TextSelectionHandle::new(text, cx));
                (handle.clone(), handle)
            }),
            None => TextSelectionHandle::new(text, cx),
        };
        (self.child.request_layout(window, cx), handle)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.child.prepaint(window, cx);
        let registration = TextSelectionRegistration::new(hitbox, bounds)
            .with_document_order(self.order)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(handle, window, cx);
        handle.register(registration, window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let run = TextSelectionRun::new(self.text.clone(), self.layout.clone(), bounds).with_document_order(self.order);
        let projection = handle.update_runs(&[run], cx);
        let range = projection.ranges().first().cloned().flatten().filter(|r| r.start < r.end && r.end <= self.text.len());
        let mut piece = None;
        if let Some(r) = range {
            let color = crate::roles::look(cx).accent().opacity(0.28);
            let line = self.layout.line_height();
            if let (Some(start), Some(end)) = (self.layout.position_for_index(r.start), self.layout.position_for_index(r.end)) {
                for b in selection_quads(start, end, self.layout.bounds(), line) {
                    window.paint_quad(fill(b, color));
                }
                piece = self.text.get(r).map(|t| (t.to_owned(), point(end.x, end.y + line)));
            }
        }
        self.child.paint(window, cx);
        let id = handle.entity_id();
        let changed = PIECES.with(|p| {
            let mut p = p.borrow_mut();
            match piece {
                Some((text, end)) => {
                    let same = p.get(&id).is_some_and(|o| o.text == text && o.end == end);
                    let (letter, order) = (self.letter.clone(), self.order);
                    p.insert(id, Piece { handle: handle.clone(), letter, order, text, end });
                    !same
                }
                None => p.remove(&id).is_some(),
            }
        });
        // Кнопка над выделением рисуется областью чтения: перерисовать её следующим кадром.
        if changed {
            let view = self.view;
            window.on_next_frame(move |_, cx| cx.notify(view));
        }
    }
}

/// Прямоугольники выделения от `start` до `end` по строкам раскладки.
fn selection_quads(start: Point<Pixels>, end: Point<Pixels>, bounds: Bounds<Pixels>, line: Pixels) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(start, point(end.x, end.y + line))];
    }
    let mut out = vec![Bounds::from_corners(start, point(bounds.right(), start.y + line))];
    if end.y > start.y + line {
        out.push(Bounds::from_corners(point(bounds.left(), start.y + line), point(bounds.right(), end.y)));
    }
    out.push(Bounds::from_corners(point(bounds.left(), end.y), point(end.x, end.y + line)));
    out
}

// --- область чтения: кнопка над выделением и меню правой кнопки ---

/// Область чтения: меню правой кнопки над выделением и кнопка «Ответить на фрагмент» у его конца.
pub(crate) fn decorate(main: Stateful<Div>, menu: Option<Point<Pixels>>, cx: &mut Context<ReadingPane>) -> Stateful<Div> {
    let l = crate::roles::look(cx);
    let sel = picked(cx);
    let menu = menu.filter(|_| sel.is_some());
    let main = main.on_mouse_down(
        MouseButton::Right,
        cx.listener(|this, e: &MouseDownEvent, _, cx| {
            let at = picked(cx).map(|_| e.position);
            if this.fragment_menu != at {
                this.fragment_menu = at;
                cx.notify();
            }
        }),
    );
    let Some(sel) = sel else { return main };
    if let Some(at) = menu {
        return main.child(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(context_menu(sel, &l, cx))).with_priority(2));
    }
    if sel.selecting {
        return main;
    }
    let at = point(sel.end.x, sel.end.y + px(4.));
    main.child(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(reply_button(sel, &l))).with_priority(2))
}

/// «Ответить на фрагмент» над выделением. Нажатие мыши снимает выделение окна раньше щелчка — действует нажатие.
fn reply_button(sel: Picked, l: &Look) -> Stateful<Div> {
    crate::buttons::secondary("fragment-reply", l)
        .h(px(28.))
        .px(px(10.))
        .occlude()
        .shadow_md()
        .text_size(px(12.))
        .child(icon(glyph::REPLY, 12.))
        .child(tr!("fragments-reply"))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            stash(sel.clone());
            window.dispatch_action(Box::new(mail::ReplyFragment), cx);
        })
}

fn context_menu(sel: Picked, l: &Look, cx: &mut Context<ReadingPane>) -> Stateful<Div> {
    let copy = sel.text.clone();
    let item = |id: &'static str, g: char, text: String| {
        crate::composer::menu_item(ElementId::from(id), l, false).child(icon(g, 14.).text_color(l.text2())).child(text)
    };
    div()
        .id("fragment-menu")
        .min_w(px(220.))
        .occlude()
        .flex()
        .flex_col()
        .p(px(4.))
        .rounded(l.px(Metric::RadiusOverlay))
        .border_1()
        .border_color(l.c(Role::StrokeCard))
        .bg(l.c(Role::LayerOverlay))
        .text_color(l.text())
        .text_size(px(13.))
        .shadow_lg()
        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.fragment_menu = None;
            cx.notify();
        }))
        .child(item("fragment-menu-reply", glyph::REPLY, tr!("fragments-reply")).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.fragment_menu = None;
                stash(sel.clone());
                window.dispatch_action(Box::new(mail::ReplyFragment), cx);
                cx.notify();
            }),
        ))
        .child(item("fragment-menu-copy", glyph::DRAFT, tr!("fragments-copy")).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.fragment_menu = None;
                crate::screens::to_clipboard(copy.clone(), cx);
                cx.notify();
            }),
        ))
}

// --- экран почты: команда `mail.reply_fragment` ---

/// Действия части: `mail.reply_fragment` — у экрана почты; команды реплик — у окна письма ([`actions`]).
pub(crate) mod mail {
    gpui_kit::actions!(mail, [ReplyFragment]);
}

/// Команды реплик письма.
pub(crate) mod compose {
    gpui_kit::actions!(compose, [FragmentUp, FragmentDown, FragmentRemove, FragmentRestore]);
}

fn handlers(root: Div, cx: &mut Context<Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &mail::ReplyFragment, window, cx| this.reply_fragment(window, cx)))
}

/// Подпись цитаты: имя отправителя, без него — адрес.
fn who(row: &EmailRow) -> String {
    if row.from_name.trim().is_empty() { row.from_addr.clone() } else { row.from_name.clone() }
}

impl Mailer {
    /// `mail.reply_fragment`: выделенный текст письма — репликой в ответ на эту беседу; ответа нет — новый ответ.
    pub(crate) fn reply_fragment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = take_stash().or_else(|| picked(cx)) else {
            return self.toast(Toast::Info, tr!("fragments-select-text"), cx);
        };
        self.reading.update(cx, |r, cx| {
            if r.fragment_menu.take().is_some() {
                cx.notify();
            }
        });
        let reading = self.reading.read(cx);
        let feed = reading.feed.as_ref().filter(|_| reading.feed_on);
        let current = reading.reading.as_ref().map(|r| r.row.clone());
        let mut letters: Vec<EmailRow> = current.iter().cloned().collect();
        if let Some(f) = feed {
            letters.extend(f.conv.messages.iter().map(|m| m.row.clone()));
        }
        let Some(row) = letters.iter().find(|r| r.id.as_str() == sel.letter.as_ref()).cloned() else {
            return self.toast(Toast::Info, tr!("fragments-select-text"), cx);
        };
        let fragment = Fragment::new(row.id.clone(), who(&row), crate::actions::full_date(row.received_at), sel.text);
        let ids: Vec<&str> = letters.iter().map(|r| r.id.as_str()).collect();
        if let Some((view, inline)) = self.composer_for(&ids, cx) {
            view.update(cx, |c, cx| c.add_fragment(fragment, window, cx));
            if !inline {
                self.toast(Toast::Info, tr!("fragments-added"), cx);
            }
            return;
        }
        // Лента беседы без открытого ответа: фрагмент — в быстрый ответ под лентой (MSGR-02).
        if feed.is_some() {
            return self.reading.update(cx, |r, cx| r.quick_add_fragment(fragment, window, cx));
        }
        let letter = row.id.clone();
        self.fragments.pending = Some(Prepared { letter, fragments: vec![fragment], ..Default::default() });
        self.respond_to(row, Mode::Reply, cx);
    }

    /// Ответ на эту беседу, который уже открыт: в области чтения над письмом беседы или вкладкой с её репликами.
    pub(crate) fn composer_for(&self, letters: &[&str], cx: &App) -> Option<(Entity<Composer>, bool)> {
        if let Some(i) = &self.compose.inline
            && letters.contains(&i.letter.as_str())
            && let Some(v) = self.compose.get(i.id)
        {
            return Some((v.clone(), true));
        }
        self.compose
            .open
            .iter()
            .find(|o| o.view.read(cx).model.fragments.list.items().iter().any(|f| letters.contains(&f.letter.as_str())))
            .map(|o| (o.view.clone(), false))
    }
}

// --- окно письма: реплики ---

/// Поля ответов реплик и что с ними сейчас.
#[derive(Default)]
pub(crate) struct Board {
    answers: HashMap<u64, (Entity<TextareaState>, Subscription)>,
    /// Фокус — в поле ответа этой реплики при следующей отрисовке.
    focus: Option<u64>,
    /// Только что убрали реплику: строка «Фрагмент убран · Вернуть».
    removed: bool,
    /// Поля уже создавались: письмо, открытое с репликой, ставит фокус в её ответ один раз.
    started: bool,
}

impl Composer {
    /// Реплика в письмо: фокус — в поле ответа на неё.
    pub(crate) fn add_fragment(&mut self, f: Fragment, window: &mut Window, cx: &mut Context<Self>) {
        let (id, split) = self.model.add_fragment(f);
        self.sync_inputs |= split;
        self.board.focus = Some(id);
        self.board.removed = false;
        self.schedule_autosave(cx);
        window.refresh();
        cx.notify();
    }

    fn remove_fragment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(joined) = self.model.remove_fragment(id) else { return };
        self.sync_inputs |= joined;
        self.board.removed = true;
        // Ctrl+Z вне поля ввода — вернуть реплику (`compose.fragment_restore`).
        window.focus(&self.focus, cx);
        self.schedule_autosave(cx);
        cx.notify();
    }

    /// `compose.fragment_restore`: нечего вернуть — Ctrl+Z дальше по цепочке.
    fn restore_fragment(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((id, split)) = self.model.restore_fragment() else { return false };
        self.sync_inputs |= split;
        self.board.focus = Some(id);
        self.board.removed = false;
        self.schedule_autosave(cx);
        cx.notify();
        true
    }

    fn move_fragment(&mut self, id: u64, delta: isize, cx: &mut Context<Self>) {
        if self.model.fragments.list.shift(id, delta) {
            self.board.focus = Some(id);
            self.schedule_autosave(cx);
            cx.notify();
        }
    }

    fn drop_fragment(&mut self, id: u64, to: usize, cx: &mut Context<Self>) {
        if self.model.fragments.list.place(id, to) {
            self.schedule_autosave(cx);
            cx.notify();
        }
    }

    /// Реплика, в поле ответа которой фокус; нет — последняя.
    fn current_fragment(&self, window: &Window, cx: &App) -> Option<u64> {
        let items = self.model.fragments.list.items();
        items
            .iter()
            .map(|f| f.id)
            .find(|id| self.board.answers.get(id).is_some_and(|(s, _)| s.focus_handle(cx).contains_focused(window, cx)))
            .or_else(|| items.last().map(|f| f.id))
    }
}

/// Поля ответов — под реплики письма (перед отрисовкой окна письма): новые создаются, убранных — нет.
pub(crate) fn prepare(c: &mut Composer, window: &mut Window, cx: &mut Context<Composer>) {
    let ids: Vec<(u64, String)> = c.model.fragments.list.items().iter().map(|f| (f.id, f.answer.clone())).collect();
    if !std::mem::replace(&mut c.board.started, true) && c.board.focus.is_none() {
        c.board.focus = ids.last().map(|(id, _)| *id);
    }
    c.board.answers.retain(|id, _| ids.iter().any(|(i, _)| i == id));
    for (id, answer) in ids {
        if c.board.answers.contains_key(&id) {
            continue;
        }
        let state =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 8).placeholder(tr!("fragments-answer-placeholder")).default_value(answer));
        let sub = cx.subscribe_in(&state, window, move |this: &mut Composer, state, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                let text = state.read(cx).value().to_string();
                if this.model.fragments.list.set_answer(id, &text) {
                    this.board.removed = false;
                    this.schedule_autosave(cx);
                    cx.notify();
                }
            }
        });
        c.board.answers.insert(id, (state, sub));
    }
    if let Some(id) = c.board.focus.take()
        && let Some((state, _)) = c.board.answers.get(&id)
    {
        state.update(cx, |s, cx| s.focus(window, cx));
    }
}

/// Команды реплик на корне окна письма.
pub(crate) fn actions(root: Div, cx: &mut Context<Composer>) -> Div {
    root.on_action(cx.listener(|this, _: &compose::FragmentUp, window, cx| {
        if let Some(id) = this.current_fragment(window, cx) {
            this.move_fragment(id, -1, cx);
        }
    }))
    .on_action(cx.listener(|this, _: &compose::FragmentDown, window, cx| {
        if let Some(id) = this.current_fragment(window, cx) {
            this.move_fragment(id, 1, cx);
        }
    }))
    .on_action(cx.listener(|this, _: &compose::FragmentRemove, window, cx| {
        if let Some(id) = this.current_fragment(window, cx) {
            this.remove_fragment(id, window, cx);
        }
    }))
    .on_action(cx.listener(|this, _: &compose::FragmentRestore, _, cx| {
        if !this.restore_fragment(cx) {
            cx.propagate();
        }
    }))
}

/// Перетаскиваемая реплика: её номер и начало текста — подпись у курсора.
#[derive(Clone)]
struct Dragged {
    id: u64,
    text: SharedString,
}

impl Render for Dragged {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        div()
            .max_w(px(320.))
            .px(px(10.))
            .py(px(4.))
            .rounded(l.px(Metric::RadiusControl))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .text_color(l.text2())
            .text_size(px(12.))
            .shadow_md()
            .truncate()
            .child(self.text.clone())
    }
}

/// Поле своего текста и под ним `under` (подсказка упоминания); с репликами поле невысокое, под ним реплики и подпись.
pub(crate) fn split_view(c: &Composer, head: AnyElement, under: Option<AnyElement>, cx: &mut Context<Composer>) -> AnyElement {
    let col = div().flex_1().min_h_0().flex().flex_col();
    if !c.model.has_fragments() && !c.board.removed {
        return col.child(head).children(under).into_any_element();
    }
    let l = crate::roles::look(cx);
    if !c.model.has_fragments() {
        return col.child(head).children(under).child(removed_line(&l, cx)).into_any_element();
    }
    col.child(div().h(px(HEAD_H)).flex_none().flex().flex_col().child(head)).children(under).child(list(c, &l, cx)).into_any_element()
}

fn removed_line(l: &Look, cx: &mut Context<Composer>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(16.))
        .py(px(6.))
        .border_t_1()
        .border_color(l.border())
        .text_color(l.text2())
        .child(tr!("fragments-removed"))
        .child(
            crate::composer::quiet_plain("fragment-restore", l)
                .h(px(26.))
                .text_color(l.accent_text())
                .hover(|d| d.bg(l.hover()))
                .child(icon('\u{E7A7}', 12.))
                .child(tr!("fragments-restore"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.restore_fragment(cx);
                })),
        )
}

fn list(c: &Composer, l: &Look, cx: &mut Context<Composer>) -> Stateful<Div> {
    let items = c.model.fragments.list.items();
    let n = items.len();
    let mut col = div().id("fragments").children(crate::screens::spot("fragments")).flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
    for (i, f) in items.iter().enumerate() {
        col = col.child(pair(c, i, n, f, l, cx));
    }
    if c.board.removed {
        col = col.child(removed_line(l, cx));
    }
    col.child(tail_view(c, l))
}

/// Реплика: ручка, цитата с подписью, ↑ ↓ ×, поле ответа.
fn pair(c: &Composer, i: usize, n: usize, f: &Fragment, l: &Look, cx: &mut Context<Composer>) -> Stateful<Div> {
    let id = f.id;
    let caption = if f.date.is_empty() { f.who.clone() } else { tr!("fragments-caption", who = f.who.clone(), date = f.date.clone()) };
    let button = |key: &'static str, g: char, tip: String, on: bool| {
        crate::composer::quiet_plain((key, id as usize), l)
            .size(px(26.))
            .px_0()
            .justify_center()
            .text_color(if on { l.text2() } else { l.text3() })
            .when(on, |d| d.hover(|d| d.bg(l.hover())))
            .child(icon(g, 12.))
            .tooltip(move |window, cx| crate::screens::tip(tip.clone(), window, cx))
    };
    let preview = SharedString::from(f.text.lines().next().unwrap_or_default().to_owned());
    let accent = l.accent();
    let answer = c.board.answers.get(&id).map(|(s, _)| Textarea::new(s).into_any_element());
    div()
        .id(("fragment", id as usize))
        .children(crate::screens::spot(("fragment", i)))
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(16.))
        .py(px(8.))
        .border_t_1()
        .border_color(l.border())
        .drag_over::<Dragged>(move |s, _, _, _| s.border_t_2().border_color(accent))
        .on_drop(cx.listener(move |this, d: &Dragged, _, cx| this.drop_fragment(d.id, i, cx)))
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(8.))
                .child(
                    div()
                        .id(("fragment-drag", id as usize))
                        .pt(px(2.))
                        .cursor_grab()
                        .text_color(l.text3())
                        .child(icon(HANDLE, 12.))
                        .on_drag(Dragged { id, text: preview }, |d, _, _, cx| cx.new(|_| d.clone()))
                        .tooltip(|window, cx| crate::screens::tip(tr!("fragments-drag"), window, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .pl(px(10.))
                        .border_l_2()
                        .border_color(l.c(Role::StrokeStrong))
                        .child(div().text_size(px(12.)).text_color(l.text2()).child(caption))
                        .child(div().max_h(px(88.)).overflow_hidden().text_color(l.text2()).child(f.text.clone())),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(2.))
                        .child(button("fragment-up", UP, tr!("cmd-compose-fragment-up"), i > 0).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.move_fragment(id, -1, cx);
                        })))
                        .child(button("fragment-down", glyph::CHEVRON_DOWN, tr!("cmd-compose-fragment-down"), i + 1 < n).on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.move_fragment(id, 1, cx);
                            },
                        )))
                        .child(button("fragment-remove", glyph::CLOSE, tr!("fragments-remove"), true).on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.remove_fragment(id, window, cx);
                            },
                        ))),
                ),
        )
        .child(div().pl(px(20.)).children(answer))
}

/// Подпись и цитата ответа под репликами — как уйдут в письме, приглушённо.
fn tail_view(c: &Composer, l: &Look) -> Div {
    let tail = &c.model.fragments.tail;
    let sig = c.model.signature.trim();
    let quote = tail.trim_start().strip_prefix(sig).unwrap_or(tail).trim();
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(16.))
        .py(px(8.))
        .border_t_1()
        .border_color(l.border())
        .text_color(l.text3())
        .when(!sig.is_empty() && tail.contains(sig), |d| d.child(div().child(sig.to_owned())))
        .when(!quote.is_empty(), |d| d.child(div().text_size(px(12.)).child(tr!("fragments-tail-quote"))))
}

// --- опись части ---

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "fragments", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/fragments.ftl")), ("en", include_str!("../i18n/en/fragments.ftl"))];

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{COMPOSER, MAIL, cmd};
    &[
        // Не Outlook (мессенджеры): кнопкой над выделением, меню правой кнопки, палитрой — без клавиши.
        cmd("mail.reply_fragment", &[], MAIL),
        // Порядок реплик — ещё и перетаскиванием за ручку; «×» — кнопкой у реплики (решение владельца: не забоем).
        cmd("compose.fragment_up", &[], COMPOSER),
        cmd("compose.fragment_down", &[], COMPOSER),
        cmd("compose.fragment_remove", &[], COMPOSER),
        // Вернуть убранную реплику — Ctrl+Z вне поля ввода (в поле это отмена правки текста).
        cmd("compose.fragment_restore", &["ctrl-z"], Some("Composer && !Input")),
    ]
};

/// Сцены `--screens` (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Input, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Письмо с картинкой и цитатой (как `reply-inline`): строка его текста выделена протяжкой мыши — подсветка и
        // кнопка «Ответить на фрагмент» под концом выделения.
        Scene::new("fragment-select", letter, letter_ready).after("reply-inline").input(SELECT),
        // Кнопка нажата: фрагмент — в быстрый ответ под лентой, набран ответ, второй фрагмент того же письма;
        // «Развернуть» — ответ всем в области чтения с двумя репликами, набранное — ответ на последнюю.
        Scene::new("reply-fragments", stay, fragments_ready).continues().input(REPLY),
    ];

    const SELECT: &[Input] = &[Input::Until(letter_ready), Input::Drag(from, to), Input::Until(selected)];
    // В ленте без открытого ответа фрагменты встают в быстрый ответ под лентой (MSGR-02); «Развернуть» — полный ответ.
    const REPLY: &[Input] = &[
        Input::Press("fragment-reply"),
        Input::Until(one_fragment),
        Input::Text(ANSWER),
        Input::Until(answered),
        Input::Run(second),
        Input::Until(two_fragments),
        Input::Press("quick-expand"),
    ];

    const ANSWER: &str = "Да, к пятнице успеем."; // i18n-ok: ввод сценария

    fn answered(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.reading.read(cx).quick.text(cx) == ANSWER
    }

    fn letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Ответ прошлых сцен над письмом (`reply-inline`) закрыт: текст письма — на виду, сверху.
        if let Some(id) = m.compose.inline.as_ref().map(|i| i.id) {
            m.close_composer(id, cx);
        }
        let Some(i) = screens::demo_row(m.list.read(cx), screens::outlook_letter) else { return Err("no message to quote".into()) };
        m.reading.update(cx, |r, _| r.scroll.set_offset(Point::default()));
        m.tabs.activate_kind(crate::tabs::TabKind::Main);
        m.set_section(Section::Mail, cx);
        m.list.update(cx, |l, cx| l.select(i, cx));
        Ok(())
    }

    fn letter_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.reading.read(cx).reading.as_ref().is_some_and(|r| r.doc.is_some()) && body().is_some()
    }

    fn stay(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
        Ok(())
    }

    /// Текст письма: обычный вид или первое письмо ленты беседы (лента — по умолчанию).
    fn body() -> Option<Bounds<Pixels>> {
        screens::spot_bounds("reading-body").or_else(|| screens::spots_numbered("feed-body-").first().copied())
    }

    /// Вторая строка текста письма (первая у образца — подпись картинки): от левого края — до 220 px правее.
    const LINE: f32 = 42.;

    fn from(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
        body().map(|b| point(b.left() + px(1.), b.top() + px(LINE)))
    }

    fn to(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
        body().map(|b| point(b.left() + px(220.), b.top() + px(LINE)))
    }

    fn selected(_: &Scene, _: &Mailer, cx: &App) -> bool {
        super::picked(cx).is_some_and(|p| !p.selecting)
    }

    fn one_fragment(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.reading.read(cx).quick.fragments.len() == 1
    }

    fn two_fragments(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.reading.read(cx).quick.fragments.len() == 2
    }

    /// Вторая реплика — как выделение из того же письма (протяжка уже показана прошлой сценой).
    fn second(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let letter = m.reading.read(cx).reading.as_ref().map(|r| r.row.id.clone()).ok_or("no letter")?;
        let text = "Бюджет согласуем отдельно.".to_owned(); // i18n-ok: образец выделения
        super::stash(super::Picked { letter: letter.into(), text, end: Point::default(), selecting: false });
        m.reply_fragment(window, cx);
        Ok(())
    }

    fn fragments_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.compose.inline_view().is_some_and(|c| {
            let c = c.read(cx);
            c.drawn() && c.model.fragments.list.len() == 2 && c.model.fragments.list.items().last().is_some_and(|f| f.answer == ANSWER)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use postman_compose::fragments::Fragment;
    use postman_compose::{Address, Draft};
    use postman_core::prefs::ReplyQuote;

    use crate::compose::Model;

    fn frag(text: &str) -> Fragment {
        Fragment::new("m1", "Анна", "17.10.2026 12:00", text)
    }

    fn row() -> postman_cache::EmailRow {
        postman_cache::EmailRow {
            id: "m1".into(),
            thread_id: "t1".into(),
            received_at: 1_792_240_200,
            subject: "План".into(),
            from_name: "Анна".into(),
            from_addr: "anna@corp.example".into(),
            mailbox_ids: vec!["inbox".into()],
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 1,
            keywords: Vec::new(),
        }
    }

    fn reply(quote: ReplyQuote) -> Model {
        let row = row();
        let body = postman_cache::BodyRow { message_id: Some("orig@corp.example".into()), ..Default::default() };
        let doc = postman_ir::from_plain("Сроки — до пятницы?\n\nБюджет — позже.");
        let me = Address::new("Я", "me@corp.example");
        let mut d = crate::actions::draft(postman_compose::Mode::Reply, &row, &body, &doc, &me, "С уважением,\nЯ", quote, |_| None);
        d.fragments = vec![frag("Сроки — до пятницы?")];
        Model::new(d, None, "С уважением,\nЯ", Instant::now())
    }

    /// Ответ на фрагмент: подпись и цитата уходят из поля под реплики; в письме — свой текст, реплики, подпись, цитата.
    #[test]
    fn reply_with_fragments_keeps_signature_below() {
        let mut m = reply(ReplyQuote::Include);
        assert!(m.is_dirty(), "ответ на фрагмент — уже изменение");
        assert_eq!(m.body, "", "в поле — только свой текст");
        assert!(m.fragments.tail.starts_with("С уважением,\nЯ\n\n"), "{}", m.fragments.tail);
        m.set(crate::compose::Field::Body, "Коллеги,".into());
        let id = m.fragments.list.items()[0].id;
        m.fragments.list.set_answer(id, "Да.");
        let md = m.markdown();
        let (q, a, s, full) =
            (md.find("> Анна, 17.10.2026 12:00:").unwrap(), md.find("Да.").unwrap(), md.find("С уважением").unwrap(), md.find("пишет:").unwrap());
        assert!(md.starts_with("Коллеги,") && q < a && a < s && s < full, "{md}");
        let d = m.draft(true).unwrap();
        assert_eq!(d.markdown, md);
    }

    /// «Не включать исходное» (#655): полной цитаты нет, фрагменты — есть.
    #[test]
    fn omit_quote_keeps_fragments() {
        let m = reply(ReplyQuote::Omit);
        let md = m.markdown();
        assert!(md.contains("> Анна, 17.10.2026 12:00:\n> Сроки — до пятницы?"), "{md}");
        assert!(!md.contains("пишет:"), "{md}");
        assert!(md.trim_end().ends_with("С уважением,\nЯ"), "{md}");
    }

    /// Убрать реплики кнопкой и вернуть: последняя убранная — подпись обратно в поле; возврат — снова под реплики.
    #[test]
    fn remove_and_restore() {
        let mut m = reply(ReplyQuote::Omit);
        let (b, _) = m.add_fragment(frag("Бюджет — позже."));
        let a = m.fragments.list.items()[0].id;
        assert_eq!(m.remove_fragment(b), Some(false));
        assert_eq!(m.remove_fragment(a), Some(true), "последняя — хвост в поле");
        assert!(!m.has_fragments() && m.fragments.tail.is_empty());
        assert!(m.body.contains("С уважением,\nЯ"), "{}", m.body);
        assert!(!m.markdown().contains('>'));
        assert_eq!(m.restore_fragment(), Some((a, true)));
        assert!(!m.body.contains("С уважением") && m.fragments.tail.contains("С уважением"));
        assert_eq!(m.restore_fragment(), Some((b, false)));
        assert_eq!(m.restore_fragment(), None);
        let texts: Vec<_> = m.fragments.list.items().iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, ["Сроки — до пятницы?", "Бюджет — позже."]);
    }

    /// Смена подписи в письме с репликами — в хвосте под ними.
    #[test]
    fn signature_swap_in_tail() {
        let mut m = reply(ReplyQuote::Omit);
        m.set_signature("Иван");
        assert_eq!(m.fragments.tail, "Иван");
        assert!(m.markdown().ends_with("Иван"));
    }

    #[test]
    fn new_letter_with_fragment_starts_with_quote() {
        let d = Draft { to: vec![Address::new("", "a@x.ru")], fragments: vec![frag("а")], ..Default::default() };
        let m = Model::new(d, None, "", Instant::now());
        assert_eq!(m.markdown(), "> Анна, 17.10.2026 12:00:\n> а");
    }
}
