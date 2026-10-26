//! Контекстное меню письма (S5-08, макет «Выделение»): правая кнопка по строке, Shift+F10 или клавиша «Меню».
//! Действует на выделение, если строка в нём, иначе — на эту строку. Не модальное окно, а слой поверх экрана:
//! стрелки — по пунктам, → / Enter — подменю («Категория», «Переместить»), ← — назад, Enter — выполнить, Esc и
//! щелчок вне — закрыть.

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_cache::EmailRow;
use postman_compose::Mode;
use postman_core::marks::Kind;

use crate::Mailer;
use crate::bulk::{Ask, key_hint, mask, menu_line, separator};
use crate::chrome::{glyph, icon};
use crate::roles::Look;

const WIDTH: f32 = 316.;
const SUB_WIDTH: f32 = 224.;
const ITEM_H: f32 = 28.;
const SEP_H: f32 = 9.;
const TITLE_H: f32 = 24.;
/// Папок в подменю «Переместить» видно сразу (дальше — прокрутка).
const SUB_MAX_H: f32 = 360.;

/// Подменю.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sub {
    Category,
    Move,
}

/// Пункт меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Reply,
    ReplyAll,
    Forward,
    Seen,
    Flag,
    Category,
    Due,
    Remind,
    Wait,
    Expire,
    Move,
    Archive,
    Delete,
    Phishing,
}

/// Пункты группами (разделитель между группами), порядок — S5-08.
const GROUPS: &[&[Item]] = &[
    &[Item::Reply, Item::ReplyAll, Item::Forward],
    &[Item::Seen, Item::Flag, Item::Category],
    &[Item::Due, Item::Remind, Item::Wait, Item::Expire],
    &[Item::Move, Item::Archive, Item::Delete],
    &[Item::Phishing],
];

fn items() -> impl Iterator<Item = Item> {
    GROUPS.iter().flat_map(|g| g.iter().copied())
}

impl Item {
    fn sub(self) -> Option<Sub> {
        match self {
            Item::Category => Some(Sub::Category),
            Item::Move => Some(Sub::Move),
            _ => None,
        }
    }

    /// Ответить можно одному письму.
    fn single(self) -> bool {
        matches!(self, Item::Reply | Item::ReplyAll | Item::Forward)
    }

    fn glyph(self) -> char {
        match self {
            Item::Reply => glyph::REPLY,
            Item::ReplyAll => glyph::REPLY_ALL,
            Item::Forward => glyph::FORWARD,
            Item::Seen => glyph::READ,
            Item::Flag => glyph::FLAG,
            Item::Category => glyph::TAG,
            Item::Due => glyph::CLOCK,
            Item::Remind => glyph::RINGER,
            Item::Wait => glyph::WAIT,
            Item::Expire => glyph::EXPIRE,
            Item::Move => glyph::MOVE,
            Item::Archive => glyph::ARCHIVE,
            Item::Delete => glyph::DELETE,
            Item::Phishing => ' ',
        }
    }

    /// Команда реестра — подсказка клавиши справа.
    fn command(self, unread: bool) -> Option<&'static str> {
        Some(match self {
            Item::Reply => "mail.reply",
            Item::ReplyAll => "mail.reply_all",
            Item::Forward => "mail.forward",
            Item::Seen if unread => "mail.mark_read",
            Item::Seen => "mail.mark_unread",
            Item::Flag => "mail.toggle_flag",
            Item::Due => "mail.due",
            Item::Remind => "mail.remind",
            Item::Wait => "mail.wait",
            Item::Expire => "mail.expire",
            Item::Move => "mail.move",
            Item::Archive => "mail.archive",
            Item::Delete => "mail.delete",
            Item::Phishing => "mail.report_phishing",
            Item::Category => return None,
        })
    }
}

/// Открытое меню.
pub(crate) struct Menu {
    /// Левый верхний угол (в окне).
    at: Point<Pixels>,
    /// Над чем: выделение или одна строка.
    ids: Vec<String>,
    /// Одна строка списка (номер и письмо): ответ, подписи «прочитано» и «флаг».
    row: Option<(usize, EmailRow)>,
    /// Есть непрочитанные / без флага — пункт ставит, иначе снимает.
    unread: bool,
    unflagged: bool,
    cursor: usize,
    sub: Option<Sub>,
    sub_cursor: usize,
    focus: FocusHandle,
    /// Фокус — меню (на следующем кадре: ему нужно окно).
    focus_pending: bool,
}

impl Menu {
    fn item(&self) -> Option<Item> {
        items().nth(self.cursor)
    }

    fn enabled(&self, it: Item) -> bool {
        !it.single() || self.row.is_some()
    }

    fn step(&mut self, delta: isize) {
        let n = items().count() as isize;
        let mut c = self.cursor as isize;
        for _ in 0..n {
            c = (c + delta).rem_euclid(n);
            if items().nth(c as usize).is_some_and(|it| self.enabled(it)) {
                break;
            }
        }
        self.cursor = c as usize;
    }
}

impl Mailer {
    /// Меню у строки `index` (правая кнопка — `at`, с клавиатуры — у строки).
    pub(crate) fn open_menu_at_row(&mut self, index: usize, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let list = self.list.read(cx);
        let Some(row) = list.row(index).cloned() else { return };
        let at = at.or_else(|| list.row_point(index)).unwrap_or_else(|| point(px(400.), px(300.)));
        let in_selection = list.selection.contains(&row.id);
        let (ids, one, unread, unflagged) = if in_selection && list.selection.len() > 1 {
            let s = list.summary.as_ref().map(|s| s.summary.clone()).unwrap_or_default();
            (list.selection.ids().to_vec(), None, s.unread > 0 || s.count == 0, s.flagged < s.count || s.count == 0)
        } else {
            // Строка беседы (вид «как Outlook», S5-05б) — над всей беседой; ответ — на это письмо.
            (list.members_of(index), Some((index, row.clone())), !row.seen, !row.flagged)
        };
        self.list.update(cx, |l, cx| {
            l.menu_row = Some(index);
            l.bulk_menu = None;
            cx.notify();
        });
        self.open_menu(at, ids, one, unread, unflagged, None, cx);
    }

    /// Меню над письмами `ids` у точки `at`; `sub` — сразу с подменю (кнопки сводки «В папку», «Категория»).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_menu(
        &mut self,
        at: Point<Pixels>,
        ids: Vec<String>,
        row: Option<(usize, EmailRow)>,
        unread: bool,
        unflagged: bool,
        sub: Option<Sub>,
        cx: &mut Context<Self>,
    ) {
        let cursor = sub.and_then(|s| items().position(|it| it.sub() == Some(s))).unwrap_or(0);
        let mut menu = Menu { at, ids, row, unread, unflagged, cursor, sub, sub_cursor: 0, focus: cx.focus_handle(), focus_pending: true };
        if !menu.item().is_some_and(|it| menu.enabled(it)) {
            menu.step(1);
        }
        self.folder_menu = None;
        self.menu = Some(menu);
        cx.notify();
    }

    /// `mail.context_menu` (Shift+F10, «Меню»): у выбранной строки; выделено несколько — над выделением.
    pub(crate) fn context_menu_key(&mut self, cx: &mut Context<Self>) {
        let list = self.list.read(cx);
        let index = list.selected.or_else(|| list.selection.anchor());
        match index {
            Some(i) => self.open_menu_at_row(i, None, cx),
            None => self.toast(pets_ui::Level::Info, tr!("actions-select-letter"), cx),
        }
    }

    /// `mail.move` (Ctrl+Shift+V): меню с открытым подменю «Переместить».
    pub(crate) fn move_key(&mut self, cx: &mut Context<Self>) {
        self.context_menu_key(cx);
        if let Some(m) = &mut self.menu {
            m.sub = Some(Sub::Move);
            m.cursor = items().position(|it| it == Item::Move).unwrap_or(0);
            m.sub_cursor = 0;
        }
    }

    /// Снимки (`--screens`): подменю открыто, как по наведению.
    pub(crate) fn screen_menu_sub(&mut self, sub: Sub) {
        if let Some(m) = &mut self.menu {
            m.sub = Some(sub);
            m.cursor = items().position(|it| it.sub() == Some(sub)).unwrap_or(0);
            m.sub_cursor = 0;
        }
    }

    /// Снимки: курсор — на последнем пункте («Сообщить о фишинге»).
    pub(crate) fn screen_menu_last(&mut self) {
        if let Some(m) = &mut self.menu {
            m.cursor = items().count() - 1;
            m.sub = None;
        }
    }

    /// Закрыть меню; `true` — было открыто (Esc идёт дальше, если нет).
    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(_) = self.menu.take() else { return false };
        self.refocus = true;
        self.list.update(cx, |l, cx| {
            l.menu_row = None;
            cx.notify();
        });
        cx.notify();
        true
    }

    /// Пункт выбран (щелчком или Enter).
    fn choose_item(&mut self, it: Item, cx: &mut Context<Self>) {
        let Some(m) = &self.menu else { return };
        if !m.enabled(it) {
            return;
        }
        if let Some(s) = it.sub() {
            if let Some(m) = &mut self.menu {
                m.sub = if m.sub == Some(s) { None } else { Some(s) };
                m.sub_cursor = 0;
                m.cursor = items().position(|x| x == it).unwrap_or(m.cursor);
            }
            return cx.notify();
        }
        let (ids, row, unread, unflagged) = (m.ids.clone(), m.row.clone(), m.unread, m.unflagged);
        self.close_menu(cx);
        let respond = |mode: Mode, this: &mut Mailer, cx: &mut Context<Mailer>| {
            if let Some((i, _)) = row.clone() {
                this.list.update(cx, |l, cx| l.select(i, cx));
                this.respond(mode, cx);
            }
        };
        match it {
            Item::Reply => respond(Mode::Reply, self, cx),
            Item::ReplyAll => respond(Mode::ReplyAll, self, cx),
            Item::Forward => respond(Mode::Forward, self, cx),
            Item::Seen => self.ask(ids, Ask::Seen(Some(unread)), cx),
            Item::Flag => self.ask(ids, Ask::Flag(Some(unflagged)), cx),
            Item::Due => self.ask(ids, Ask::Lifecycle(Kind::Due), cx),
            Item::Remind => self.ask(ids, Ask::Lifecycle(Kind::Remind), cx),
            Item::Wait => self.ask(ids, Ask::Lifecycle(Kind::Wait), cx),
            Item::Expire => self.ask(ids, Ask::Lifecycle(Kind::Expire), cx),
            Item::Archive => self.ask(ids, Ask::Archive, cx),
            Item::Delete => self.ask(ids, Ask::Trash, cx),
            Item::Phishing => self.ask(ids, Ask::Phishing, cx),
            Item::Category | Item::Move => {}
        }
    }

    /// Пункт подменю: категория (последний — «Снять категорию») или папка.
    fn choose_sub(&mut self, k: usize, cx: &mut Context<Self>) {
        let Some(m) = &self.menu else { return };
        let Some(sub) = m.sub else { return };
        let ids = m.ids.clone();
        let list = self.list.read(cx);
        let ask = match sub {
            Sub::Category => {
                let cats = list.category_items();
                match cats.get(k) {
                    Some(c) => Ask::Category(c.0.clone()),
                    None => Ask::Uncategorize,
                }
            }
            Sub::Move => match list.folder_items().get(k) {
                Some((id, _)) => Ask::Move(id.clone()),
                None => return,
            },
        };
        self.close_menu(cx);
        self.ask(ids, ask, cx);
    }

    fn sub_len(&self, sub: Sub, cx: &App) -> usize {
        let list = self.list.read(cx);
        match sub {
            Sub::Category => list.category_items().len() + 1,
            Sub::Move => list.folder_items().len(),
        }
    }

    /// Клавиши в меню: стрелки, Home/End, Enter и пробел — выполнить, → — подменю, ← — назад.
    fn menu_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(m) = &self.menu else { return };
        let key = ev.keystroke.key.as_str();
        let sub = m.sub;
        let in_sub = sub.is_some() && m.item().and_then(Item::sub) == sub;
        let sub_n = sub.map_or(0, |s| self.sub_len(s, cx));
        let Some(m) = &mut self.menu else { return };
        match key {
            "down" | "up" if in_sub && sub_n > 0 => {
                let d: isize = if key == "down" { 1 } else { -1 };
                m.sub_cursor = (m.sub_cursor as isize + d).rem_euclid(sub_n as isize) as usize;
            }
            "down" => m.step(1),
            "up" => m.step(-1),
            "home" => {
                m.cursor = items().count() - 1;
                m.step(1);
            }
            "end" => {
                m.cursor = 0;
                m.step(-1);
            }
            "right" => {
                if let Some(s) = m.item().and_then(Item::sub) {
                    m.sub = Some(s);
                    m.sub_cursor = 0;
                }
            }
            "left" => m.sub = None,
            "enter" | "space" => {
                if in_sub {
                    let k = m.sub_cursor;
                    return self.choose_sub(k, cx);
                }
                if let Some(it) = m.item() {
                    return self.choose_item(it, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Меню поверх экрана (последним перед тостами).
    pub(crate) fn context_menu(&mut self, window: &mut Window, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.menu.as_mut()?;
        if std::mem::take(&mut m.focus_pending) {
            window.focus(&m.focus, cx);
        }
        let m = self.menu.as_ref()?;
        let viewport = window.viewport_size();
        // Над выделением — строка «Выбрано N писем» сверху.
        let title_h = if m.ids.len() > 1 { TITLE_H } else { 0. };
        let height = items().count() as f32 * ITEM_H + (GROUPS.len() - 1) as f32 * SEP_H + 8. + title_h;
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let x = f32::from(m.at.x).min(vw - WIDTH - 8.).max(8.);
        let y = f32::from(m.at.y).min(vh - height - 8.).max(8.);
        // Подменю — справа, если помещается, иначе слева.
        let sub_right = x + WIDTH + 6. + SUB_WIDTH < vw - 8.;
        let list = self.list.read(cx);
        let cats = list.category_items();
        let folders = list.folder_items();
        let many = m.ids.len();
        let mut col = div().flex().flex_col();
        let mut index = 0usize;
        let mut top = 4. + title_h;
        let mut sub_top = None;
        for (g, group) in GROUPS.iter().enumerate() {
            if g > 0 {
                col = col.child(separator(l));
                top += SEP_H;
            }
            for &it in group.iter() {
                let i = index;
                index += 1;
                let enabled = m.enabled(it);
                let on = m.cursor == i;
                if on && it.sub().is_some() && m.sub == it.sub() {
                    sub_top = Some(top);
                }
                top += ITEM_H;
                let label = match it {
                    Item::Reply => tr!("menu-reply"),
                    Item::ReplyAll => tr!("menu-reply-all"),
                    Item::Forward => tr!("menu-forward"),
                    Item::Seen if m.unread => tr!("menu-read"),
                    Item::Seen => tr!("menu-unread"),
                    Item::Flag if m.unflagged => tr!("menu-flag"),
                    Item::Flag => tr!("menu-unflag"),
                    Item::Category => tr!("menu-category"),
                    Item::Due => tr!("menu-due"),
                    Item::Remind => tr!("menu-remind"),
                    Item::Wait => tr!("menu-wait"),
                    Item::Expire => tr!("menu-expire"),
                    Item::Move => tr!("menu-move"),
                    Item::Archive => tr!("menu-archive"),
                    Item::Delete => tr!("menu-delete"),
                    Item::Phishing => tr!("menu-phishing"),
                };
                let key = it.command(m.unread).map(|c| key_hint(c, cx)).unwrap_or_default();
                let glyph_el = if it == Item::Phishing {
                    // Прорези маски — цветом слоя меню (заливка наведения полупрозрачная).
                    mask(16., l, l.c(Role::LayerOverlay)).into_any_element()
                } else {
                    icon(it.glyph(), 16.).text_color(if enabled { l.text2() } else { l.c(Role::TextDisabled) }).into_any_element()
                };
                col = col.child(
                    menu_line(("menu-item", i), l)
                        .when(on, |d| d.bg(l.hover()))
                        .when(!enabled, |d| d.text_color(l.c(Role::TextDisabled)).cursor_default())
                        .child(glyph_el)
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .when(!key.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(l.text3()).child(key)))
                        .when(it.sub().is_some(), |d| d.child(icon(glyph::CHEVRON_RIGHT, 10.).text_color(l.text2())))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if let Some(m) = &mut this.menu
                                && *hovered
                                && m.cursor != i
                            {
                                m.cursor = i;
                                // Наведение на пункт с подменю открывает его, на другой — закрывает.
                                m.sub = it.sub();
                                m.sub_cursor = 0;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_item(it, cx))),
                );
            }
        }
        let title = (many > 1).then(|| {
            div().h(px(TITLE_H)).px(px(8.)).flex().items_center().text_size(px(12.)).text_color(l.text2()).child(tr!("menu-on-selection", n = many))
        });
        // Где подменю в окне: щелчок по нему — не «мимо меню» (оно вне границ самого меню).
        let sub_bounds = m.sub.zip(sub_top).map(|(s, at)| {
            let n = match s {
                Sub::Category => cats.len() + 1,
                Sub::Move => folders.len(),
            };
            let extra = if s == Sub::Category { SEP_H } else { 0. };
            let h = (n as f32 * ITEM_H + extra + 8.).min(SUB_MAX_H);
            let left = if sub_right { x + WIDTH + 6. } else { x - SUB_WIDTH - 6. };
            Bounds::new(point(px(left), px(y + at - 5.)), size(px(SUB_WIDTH), px(h)))
        });
        let sub = m.sub.zip(sub_top).map(|(s, at)| {
            let mut sc = div().flex().flex_col();
            match s {
                Sub::Category => {
                    for (k, (_, name, color, key)) in cats.iter().enumerate() {
                        let on = m.sub_cursor == k;
                        sc = sc.child(
                            menu_line(("menu-cat", k), l)
                                .when(on, |d| d.bg(l.hover()))
                                .child(div().size(px(12.)).flex_none().rounded(px(3.)).bg(l.cat(color).dot))
                                .child(div().flex_1().truncate().child(name.clone()))
                                .child(div().text_size(px(11.)).text_color(l.text3()).child(key.map(|k| k.to_string()).unwrap_or_default()))
                                .on_click(cx.listener(move |this, _, _, cx| this.choose_sub(k, cx))),
                        );
                    }
                    let k = cats.len();
                    sc = sc.child(separator(l)).child(
                        menu_line("menu-cat-none", l)
                            .when(m.sub_cursor == k, |d| d.bg(l.hover()))
                            .text_color(l.text2())
                            .child(div().size(px(12.)).flex_none().rounded(px(3.)).border_1().border_color(l.text3()))
                            .child(div().flex_1().child(tr!("bulk-uncategorize")))
                            .on_click(cx.listener(move |this, _, _, cx| this.choose_sub(k, cx))),
                    );
                }
                Sub::Move => {
                    for (k, (_, name)) in folders.iter().enumerate() {
                        let on = m.sub_cursor == k;
                        sc = sc.child(
                            menu_line(("menu-folder", k), l)
                                .when(on, |d| d.bg(l.hover()))
                                .child(icon(glyph::FOLDER, 14.).text_color(l.text2()))
                                .child(div().flex_1().truncate().child(name.clone()))
                                .on_click(cx.listener(move |this, _, _, cx| this.choose_sub(k, cx))),
                        );
                    }
                }
            }
            let left = if sub_right { WIDTH + 6. } else { -(SUB_WIDTH + 6.) };
            div()
                .id("menu-sub")
                .occlude()
                .absolute()
                .left(px(left))
                .top(px(at - 5.))
                .w(px(SUB_WIDTH))
                .max_h(px(SUB_MAX_H))
                .overflow_y_scroll()
                .p(px(4.))
                .rounded(l.px(Metric::RadiusOverlay))
                .border_1()
                .border_color(l.c(Role::StrokeCard))
                .bg(l.c(Role::LayerOverlay))
                .shadow_lg()
                .child(sc)
        });
        let focus = m.focus.clone();
        Some(
            div()
                .id("context-menu")
                .key_context("ContextMenu")
                .track_focus(&focus)
                .occlude()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(WIDTH))
                .p(px(4.))
                .rounded(l.px(Metric::RadiusOverlay))
                .border_1()
                .border_color(l.c(Role::StrokeCard))
                .bg(l.c(Role::LayerOverlay))
                .shadow_lg()
                .text_size(px(13.))
                .text_color(l.text())
                .children(title)
                .child(col)
                .children(sub)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.menu_key(ev, cx)))
                .on_mouse_down_out(cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    if sub_bounds.is_some_and(|b| b.contains(&ev.position)) {
                        return;
                    }
                    this.close_menu(cx);
                }))
                .into_any_element(),
        )
    }
}
