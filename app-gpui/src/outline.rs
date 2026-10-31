//! Беседы в списке (S5-05б, DESIGN «Беседы»): строка на беседу — самое новое письмо папки с числом писем. «Как
//! Outlook» — со стрелкой: развёрнутая беседа — письма под ней строками пониже, с отступом и линией; «Лента» и
//! «Стопка» (T1-15) — без раскрытия в списке, беседа — в панели чтения. Порядок и навигация —
//! `postman_core::thread::Outline`; здесь — его место в списке (`MailList`) и строки.
//!
//! Беседы считаются в фоне вместе со своим набором писем (`Job` в `list.rs`): видимые строки — набор `view`, j/k
//! идут по ним, ← / → и Enter сворачивают и разворачивают. Выделить строку беседы — все её письма (S5-08).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::EmailRow;
use postman_core::filters::Group;
use postman_core::prefs::Conversation;
use postman_core::thread::{Line, Outline};

use crate::chrome::{glyph, icon};
use crate::filters::Slot;
use crate::list::{MailList, PageSource, RowState};
use crate::roles::Look;

/// Высота строки письма под головой беседы (макет).
pub(crate) const CHILD_H: f32 = 30.0;
/// Где линия беседы: под стрелкой (полоса категории 4 px + половина колонки стрелки).
const GUIDE_X: f32 = 16.0;

/// Место строки в беседе.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Обычная строка: не вид «как Outlook» или беседа из одного письма.
    #[default]
    None,
    /// Голова беседы из `n` писем; `fold` — раскрывается в списке (вид «как Outlook»).
    Head { n: usize, open: bool, fold: bool },
    /// Письмо под головой; `last` — последнее в беседе.
    Child { last: bool },
}

impl Mark {
    pub(crate) fn open_head(self) -> bool {
        matches!(self, Mark::Head { open: true, .. })
    }

    /// Число писем беседы — серая «таблетка» в строке отправителя.
    pub(crate) fn count(self, l: &Look) -> Option<Div> {
        let Mark::Head { n, .. } = self else { return None };
        Some(
            div()
                .flex_none()
                .min_w(px(18.))
                .flex()
                .justify_center()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(9.))
                .bg(l.fill())
                .text_color(l.text2())
                .text_size(px(11.))
                .line_height(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(n.to_string()),
        )
    }

    /// Стрелка «Развернуть беседу» / «Свернуть беседу» слева от аватара.
    pub(crate) fn chevron(self, i: usize, l: &Look, cx: &mut Context<MailList>) -> Option<Stateful<Div>> {
        let Mark::Head { open, fold: true, .. } = self else { return None };
        let hint = if open { tr!("thread-collapse") } else { tr!("thread-expand") };
        Some(
            // Колонка стрелки — вплотную к полосе и аватару (без промежутков строки), как в макете.
            div()
                .id(("thread-chevron", i))
                .w(px(24.))
                .mx(px(-10.))
                .h_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .text_color(l.text2())
                .cursor_pointer()
                .child(
                    div()
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .hover(|d| d.bg(l.hover()))
                        .child(icon(if open { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT }, 10.)),
                )
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_thread(i, cx);
                })),
        )
    }
}

/// Линия от стрелки развёрнутой беседы вниз, к письмам под ней.
pub(crate) fn guide(l: &Look) -> Div {
    let top = crate::list::row_height(l.density()) / 2. + 8.;
    div().absolute().left(px(GUIDE_X)).top(px(top)).bottom_0().w(px(1.)).bg(l.stroke())
}

/// Строка письма под головой беседы: точка «не прочитано», отправитель, превью (нет — тема), вложение, время.
pub(crate) fn child_row(
    i: usize,
    r: &EmailRow,
    labels: crate::labels::RowLabels,
    state: RowState,
    l: &Look,
    cx: &mut Context<MailList>,
) -> AnyElement {
    let last = matches!(state.thread, Mark::Child { last: true });
    let from = if r.from_name.is_empty() { r.from_addr.clone() } else { r.from_name.clone() };
    let text = if r.preview.is_empty() { r.subject.clone() } else { r.preview.clone() };
    let lit = state.open || state.picked;
    let stripe = if lit { Some(l.row_strip(state.focused)) } else { labels.stripe.map(|c| l.cat(c).dot) };
    let line = |d: Div| d.absolute().left(px(GUIDE_X)).w(px(1.)).bg(l.stroke());
    div()
        .id(i)
        .relative()
        .w_full()
        .h(px(CHILD_H))
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(42.))
        .pr(px(12.))
        .text_size(px(12.))
        .when(last, |d| d.border_b_1().border_color(l.border()))
        .when(lit, |d| d.bg(l.row_fill(state.focused)))
        .when(!lit, |d| d.hover(|d| d.bg(l.row_hover())))
        .when(state.menu, |d| d.border_1().border_color(l.accent_text()))
        .cursor_pointer()
        .child(div().absolute().left_0().top_0().bottom_0().w(px(4.)).when_some(stripe, |d, c| d.bg(c)))
        .child(line(div()).top_0().when(last, |d| d.h(px(CHILD_H / 2.))).when(!last, |d| d.bottom_0()))
        .child(div().absolute().left(px(GUIDE_X)).top(px(CHILD_H / 2.)).w(px(18.)).h(px(1.)).bg(l.stroke()))
        .child(div().size(px(6.)).flex_none().rounded_full().when(!r.seen, |d| d.bg(l.accent_text())))
        .child(div().w(px(112.)).flex_none().truncate().text_size(px(13.)).when(!r.seen, |d| d.font_weight(FontWeight::SEMIBOLD)).child(from))
        .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).child(text))
        .when(r.has_attachment, |d| d.child(icon(glyph::ATTACH, 12.).text_color(l.text3())))
        .child(div().flex_none().text_color(l.text2()).child(crate::format_date(r.received_at)))
        .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
            let m = ev.modifiers();
            if m.shift {
                this.extend_to(i, cx);
            } else if m.control {
                this.pick(i, true, cx);
            } else {
                this.select(i, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |_, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                cx.emit(crate::bulk::OpenMenu { index: i, at: Some(ev.position) });
            }),
        )
        .into_any_element()
}

/// Строка письма под головой, которая ещё читается из хранилища.
pub(crate) fn empty_child(i: usize, l: &Look) -> AnyElement {
    div().id(i).w_full().h(px(CHILD_H)).border_b_1().border_color(l.border()).into_any_element()
}

/// Беседы набора: `ids` и `groups` — расставленный список (`arrange`), `threads` — цепочка каждого письма; «Все
/// папки» (`split`) — беседа своя у каждой папки.
pub(crate) fn outline_of(
    ids: &[String],
    groups: &[(Group, usize)],
    mut threads: HashMap<String, String>,
    split: bool,
    open: HashSet<String>,
) -> Outline {
    let mut g = 0;
    let mut left = groups.first().map_or(usize::MAX, |(_, n)| *n);
    let items = ids.iter().map(|id| {
        while left == 0 && g + 1 < groups.len() {
            g += 1;
            left = groups[g].1;
        }
        left = left.saturating_sub(1);
        (id.clone(), threads.remove(id).unwrap_or_default(), g)
    });
    let items: Vec<(String, String, usize)> = items.collect();
    Outline::build(items, split, open)
}

/// Что сделать с беседой у выбранной строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fold {
    Expand,
    Collapse,
    Toggle,
}

impl MailList {
    /// Беседы в списке открытой папки — при любом виде (T1-15; поиск — без бесед).
    pub(crate) fn outlook_mode(&self, _: &App) -> bool {
        self.search_ids.is_none() && self.mailbox.is_some()
    }

    /// Вид папки — «Лента» или «Стопка» (T1-15): строка на беседу без раскрытия в списке.
    pub(crate) fn flat_mode(&self, cx: &App) -> bool {
        self.mailbox.as_deref().is_none_or(|mb| crate::prefs::get(cx).reading.conversation_in(Some(mb)) != Conversation::Outlook)
    }

    /// Развёрнутые беседы для пересчёта: в «Ленте» и «Стопке» — ни одной.
    pub(crate) fn open_threads(&self) -> HashSet<String> {
        if self.flat { HashSet::new() } else { self.threads_open.clone() }
    }

    /// Беседы показаны: посчитаны и список — не результаты поиска (поиск — без бесед, беседы ждут возврата к папке).
    pub(crate) fn threads(&self) -> Option<&Outline> {
        self.outline.as_ref().filter(|_| self.search_ids.is_none())
    }

    /// Место строки `i` в беседе.
    pub(crate) fn thread_mark(&self, i: usize) -> Mark {
        let Some(o) = self.threads() else { return Mark::None };
        match o.line(i) {
            Some(Line::Head(h)) => match o.heads().get(h) {
                Some(head) if head.ids.len() > 1 => Mark::Head { n: head.ids.len(), open: o.is_open(head), fold: !self.flat },
                _ => Mark::None,
            },
            Some(Line::Child(h, k)) => Mark::Child { last: o.heads().get(h).is_some_and(|head| k + 1 == head.ids.len()) },
            None => Mark::None,
        }
    }

    /// Беседы посчитаны (в фоне): набор писем — видимые строки, группы — по ним.
    pub(crate) fn set_outline(&mut self, mut outline: Outline, groups: &[(Group, usize)]) {
        // Пока считали, беседу могли развернуть или свернуть.
        outline.set_open(self.open_threads());
        self.thread_groups = groups.iter().map(|(g, _)| g.clone()).collect();
        self.groups = outline.groups(&self.thread_groups);
        self.view = Some(Arc::new(outline.ids().to_vec()));
        self.outline = Some(outline);
    }

    /// Высоты элементов: письма под головой — [`CHILD_H`].
    pub(crate) fn child_sizes(&self, sizes: &mut [Size<Pixels>]) {
        let Some(o) = self.threads() else { return };
        for (item, s) in sizes.iter_mut().enumerate() {
            if let Some(Slot::Row(r)) = self.layout.slot(item)
                && matches!(o.line(r), Some(Line::Child(..)))
            {
                s.height = px(CHILD_H);
            }
        }
    }

    /// ←, → и Enter у выбранной строки. `false` — не вид «как Outlook» или делать нечего (клавиша идёт дальше).
    pub(crate) fn fold(&mut self, how: Fold, cx: &mut Context<Self>) -> bool {
        let Some(i) = self.selected.filter(|_| self.search_ids.is_none() && !self.flat) else { return false };
        let Some(o) = &mut self.outline else { return false };
        let next = match how {
            Fold::Expand => o.expand(i),
            Fold::Collapse => o.collapse(i),
            Fold::Toggle => o.toggle(i),
        };
        let Some(next) = next else { return false };
        self.outline_changed(cx);
        if self.selected != Some(next) {
            self.select(next, cx);
        }
        true
    }

    /// Щелчок по стрелке беседы. Открытое письмо скрылось в свёрнутой беседе — открыть её голову.
    pub(crate) fn toggle_thread(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.flat {
            return;
        }
        let had = self.selected.is_some();
        let Some(head) = self.outline.as_mut().and_then(|o| o.toggle(i)) else { return };
        self.outline_changed(cx);
        if had && self.selected.is_none() {
            self.select(head, cx);
        }
    }

    /// Беседа развёрнута или свёрнута: новый набор строк; уже прочитанные строки остаются на местах, недостающие —
    /// из хранилища в фоне (`ensure`). Выбранное письмо — на своей новой строке (скрылось — без выбора).
    fn outline_changed(&mut self, cx: &mut Context<Self>) {
        let Some(o) = &self.outline else { return };
        let selected = self.selected.and_then(|i| self.row(i)).map(|r| r.id.clone());
        let ids = o.ids().to_vec();
        let groups = o.groups(&self.thread_groups);
        let open = o.open().clone();
        let at = selected.and_then(|id| o.position(&id));
        let old: HashMap<String, EmailRow> = std::mem::take(&mut self.rows.1).into_iter().map(|r| (r.id.clone(), r)).collect();
        let offset = self.rows.0.min(ids.len());
        let rows: Vec<EmailRow> = ids[offset..].iter().take(old.len()).map_while(|id| old.get(id).cloned()).collect();
        self.threads_open = open;
        self.groups = groups;
        self.count = ids.len();
        self.view = Some(Arc::new(ids));
        self.rows = (offset, rows);
        self.selected = at;
        self.page_gen += 1;
        self.page_pending = None;
        self.pending_open = None;
        self.relayout();
        let visible = self.visible.clone();
        self.ensure(visible, cx);
        cx.notify();
    }

    /// Письмо ушло из списка: из своей беседы (дальше список перечитывается, `removed`).
    pub(crate) fn outline_removed(&mut self, id: &str) {
        let Some(o) = &mut self.outline else { return };
        o.remove(id);
        self.groups = o.groups(&self.thread_groups);
        self.view = Some(Arc::new(o.ids().to_vec()));
    }

    /// Письма строки `i`: у головы беседы — вся беседа, иначе — само письмо (если строка прочитана).
    pub(crate) fn members_of(&self, i: usize) -> Vec<String> {
        match self.threads() {
            Some(o) if o.members(i).len() > 1 => o.members(i).to_vec(),
            _ => self.row(i).map(|r| vec![r.id.clone()]).unwrap_or_default(),
        }
    }

    /// id видимых строк → письма: голова беседы — все письма беседы (Shift-диапазон, S5-08).
    pub(crate) fn expand_members(&self, ids: Vec<String>) -> Vec<String> {
        let Some(o) = self.threads() else { return ids };
        let heads: HashMap<&str, &[String]> =
            o.heads().iter().filter(|h| h.ids.len() > 1).filter_map(|h| Some((h.ids.first()?.as_str(), &h.ids[..]))).collect();
        ids.into_iter()
            .flat_map(|id| match heads.get(id.as_str()) {
                Some(all) => all.to_vec(),
                None => vec![id],
            })
            .collect()
    }

    /// Все письма списка (выделить все, проверка выделения): в беседах — и свёрнутые.
    pub(crate) fn alive_source(&self) -> PageSource {
        match self.threads() {
            Some(o) => PageSource::Ids(Arc::new(o.all_ids().cloned().collect())),
            None => self.page_source(),
        }
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::outline_of;
    use postman_core::filters::Group;
    use std::collections::{HashMap, HashSet};

    /// Номера групп писем — по группам расставленного списка, пустые группы пропускаются.
    #[test]
    fn outline_takes_groups_of_the_arranged_list() {
        let ids: Vec<String> = ["a2", "b", "a1"].iter().map(|s| s.to_string()).collect();
        let groups = vec![(Group::Top, 1), (Group::Folder("x".into()), 0), (Group::Folder("y".into()), 2)];
        let threads: HashMap<String, String> = [("a2", "t"), ("a1", "t")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let o = outline_of(&ids, &groups, threads.clone(), false, HashSet::new());
        assert_eq!(o.ids(), ["a2", "b"]);
        assert_eq!(o.heads().iter().map(|h| h.group).collect::<Vec<_>>(), [0, 2]);
        let names: Vec<Group> = groups.iter().map(|(g, _)| g.clone()).collect();
        assert_eq!(o.groups(&names), [(Group::Top, 1), (Group::Folder("y".into()), 1)]);
        // Без групп — всё в нулевой.
        assert_eq!(outline_of(&ids, &[], threads, true, HashSet::new()).heads()[1].group, 0);
    }

    /// Три вида на одном наборе (T1-15): «Лента» и «Стопка» — строка на беседу, самое новое письмо, без раскрытия
    /// (`MailList::open_threads` пуст); «как Outlook» с развёрнутой беседой — голова и письма под ней.
    #[test]
    fn views_shape_the_list() {
        let ids: Vec<String> = ["a3", "b", "a1"].iter().map(|s| s.to_string()).collect();
        let threads: HashMap<String, String> = [("a3", "t"), ("a1", "t")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let rows = |open: HashSet<String>| outline_of(&ids, &[], threads.clone(), false, open).ids().to_vec();
        assert_eq!(rows(HashSet::new()), ["a3", "b"], "лента и стопка");
        let outlook = rows(["t".to_string()].into());
        assert!(outlook.len() > 2 && outlook.contains(&"a1".to_string()) && outlook.ends_with(&["b".to_string()]), "как Outlook: {outlook:?}");
    }
}
