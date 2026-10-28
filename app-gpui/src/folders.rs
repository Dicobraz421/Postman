//! Панель папок (разделы — рейл модулей, `chrome.rs`). Общие ящики (S4-06) — свёртываемые ветки
//! с именем аккаунта под своими папками; своя учётная запись сворачивается так же (T1-06). Воронка у открытой папки
//! переключает панель на фильтры (S5-03, `filters.rs`). Правая кнопка по папке — её меню (`folder_menu.rs`, S5-05б).
//! Дерево прокручивается с полосой при наведении, открытая папка — в видимой области (T1-07).

use std::collections::HashSet;

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::{AccountRow, MailboxRow, account_of};

use pets_ui::theme::{Metric, Role};

use crate::filters::FilterModel;
use crate::roles::{Look, look};

// --- папки ---

pub(crate) struct Folders {
    /// Экран почты в фокусе (T1-D): выбранная папка — тоном акцента, иначе нейтральной заливкой.
    pub(crate) focused: bool,
    pub(crate) mailboxes: Vec<MailboxRow>,
    /// Общие ящики (S4-06): ветка на аккаунт.
    pub(crate) accounts: Vec<AccountRow>,
    /// Свёрнутые группы (id аккаунта; своя — [`OWN`]) — на время работы.
    pub(crate) collapsed: HashSet<String>,
    /// Свёрнутые папки с подпапками (T1-04г) — на время работы.
    folded: HashSet<String>,
    pub(crate) current: Option<String>,
    /// Число результатов, пока показан поиск.
    pub(crate) results: Option<usize>,
    /// Фильтры (S5-03): модель, общая со списком; панель на месте папок — `FilterModel::panel`.
    pub(crate) filters: Option<Entity<FilterModel>>,
    /// Своя учётная запись (логин) — подпись над своими папками; без неё (демо) подписи нет, пока есть связь.
    pub(crate) own: Option<String>,
    /// Связи с сервером нет (S5-08): метка «нет связи» у своего аккаунта.
    pub(crate) offline: bool,
    /// Прокрутка дерева (T1-07): полоса и открытая папка в видимой области.
    scroll: ScrollHandle,
    _watch: Option<Subscription>,
}

/// Ключ своей учётной записи в [`Folders::collapsed`]: id аккаунта не бывает пустым.
pub(crate) const OWN: &str = "";

/// Строка дерева папок — по порядку детей списка `folders`.
enum Row<'a> {
    /// Подпись своей учётной записи.
    Own,
    /// Ветка доп. учётки или общего ящика.
    Branch(&'a AccountRow),
    /// Папка: в ветке ли (отступ ветки), уровень вложенности, есть ли подпапки.
    Folder(&'a MailboxRow, bool, Node),
}

/// Место папки в дереве (T1-04г).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub(crate) depth: usize,
    pub(crate) parent: bool,
}

/// Дерево папок по `parent_id` (T1-04г): подпапки — под родителем, среди соседей — `sort_order`, при равном —
/// порядок сервера; родителя нет в списке — папка наверху; цикл `parent_id` не зацикливает (такие папки — наверху).
/// `folded` — свёрнутые: их подпапки не показываются.
pub(crate) fn tree<'a>(folders: &[&'a MailboxRow], folded: &HashSet<String>) -> Vec<(&'a MailboxRow, Node)> {
    let ids: HashSet<&str> = folders.iter().map(|m| m.id.as_str()).collect();
    fn parent<'m>(m: &'m MailboxRow, ids: &HashSet<&str>) -> Option<&'m str> {
        m.parent_id.as_deref().filter(|p| ids.contains(p) && *p != m.id)
    }
    let children = |of: Option<&str>| {
        let mut v: Vec<&'a MailboxRow> = folders.iter().copied().filter(|m| parent(m, &ids) == of).collect();
        v.sort_by_key(|m| m.sort_order);
        v
    };
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    // `show` — строка видна (не под свёрнутой папкой); скрытые всё равно помечаются пройденными — не «цикл».
    fn walk<'a>(
        m: &'a MailboxRow,
        depth: usize,
        show: bool,
        children: &dyn Fn(Option<&str>) -> Vec<&'a MailboxRow>,
        folded: &HashSet<String>,
        seen: &mut HashSet<&'a str>,
        out: &mut Vec<(&'a MailboxRow, Node)>,
    ) {
        if !seen.insert(m.id.as_str()) {
            return;
        }
        let kids = children(Some(m.id.as_str()));
        if show {
            out.push((m, Node { depth, parent: !kids.is_empty() }));
        }
        let open = show && !folded.contains(&m.id);
        for k in kids {
            walk(k, depth + 1, open, children, folded, seen, out);
        }
    }
    for root in children(None) {
        walk(root, 0, true, &children, folded, &mut seen, &mut out);
    }
    // Папки в цикле `parent_id` (ни одна не корень) — наверх, с их подпапками.
    let mut rest: Vec<&'a MailboxRow> = folders.iter().copied().filter(|m| !seen.contains(m.id.as_str())).collect();
    rest.sort_by_key(|m| m.sort_order);
    for m in rest {
        walk(m, 0, true, &children, folded, &mut seen, &mut out);
    }
    out
}

pub(crate) enum FolderEvent {
    Open(String),
    /// Правая кнопка по папке (S5-05б): меню папки у точки `at`.
    Menu {
        id: String,
        at: Point<Pixels>,
    },
}

impl EventEmitter<FolderEvent> for Folders {}

impl Folders {
    pub(crate) fn new(mailboxes: Vec<MailboxRow>, accounts: Vec<AccountRow>, current: Option<String>) -> Self {
        Self {
            focused: true,
            mailboxes,
            accounts,
            collapsed: HashSet::new(),
            folded: HashSet::new(),
            current,
            results: None,
            filters: None,
            own: None,
            offline: false,
            scroll: ScrollHandle::new(),
            _watch: None,
        }
    }

    /// Следить за фильтрами: панель, воронка и её счётчик перерисовываются вместе с моделью.
    pub(crate) fn watch(mut self, filters: Entity<FilterModel>, cx: &mut Context<Self>) -> Self {
        self._watch = Some(cx.observe(&filters, |_, _, cx| cx.notify()));
        self.filters = Some(filters);
        self
    }

    /// Папки аккаунта (`None` — своего) в порядке сервера.
    pub(crate) fn of(&self, account: Option<&str>) -> impl Iterator<Item = &MailboxRow> {
        self.mailboxes.iter().filter(move |m| account_of(&m.id) == account)
    }

    /// Аккаунт только для чтения: так сказала сессия или ни в одной его папке ничего нельзя изменить
    /// (Stalwart ставит `isReadOnly: false` и при папке, открытой только на чтение, — права в `myRights`).
    pub(crate) fn read_only(&self, account: &AccountRow) -> bool {
        let mut folders = self.of(Some(&account.id)).peekable();
        account.read_only || (folders.peek().is_some() && folders.all(|m| m.rights.read_only()))
    }

    /// Свернуть или развернуть подпапки папки `id` (T1-04г).
    fn fold(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.folded.remove(id) {
            self.folded.insert(id.to_owned());
        }
        cx.notify();
    }

    fn toggle(&mut self, account: &str, cx: &mut Context<Self>) {
        self.flip(account);
        cx.notify();
    }

    /// Свернуть или развернуть группу (`OWN` — свою).
    fn flip(&mut self, account: &str) {
        if !self.collapsed.remove(account) {
            self.collapsed.insert(account.to_owned());
        }
    }

    /// Открыть папку (щелчок, палитра, Ctrl+Y): её группа разворачивается, строка прокручивается в видимую область.
    pub(crate) fn open(&mut self, id: String) {
        self.collapsed.remove(account_of(&id).unwrap_or(OWN));
        // Свёрнутые папки над ней — развернуть (T1-04г); цикл `parent_id` не зацикливает.
        let mut up = self.mailboxes.iter().find(|m| m.id == id).and_then(|m| m.parent_id.clone());
        let mut steps = 0;
        while let Some(p) = up.take().filter(|_| steps < self.mailboxes.len()) {
            self.folded.remove(&p);
            up = self.mailboxes.iter().find(|m| m.id == p).and_then(|m| m.parent_id.clone());
            steps += 1;
        }
        self.current = Some(id);
        if let Some(ix) = self.current_row() {
            self.scroll.scroll_to_item(ix);
        }
    }

    /// Подпись своей учётной записи есть: логин известен или нет связи (в демо без связи — нет).
    fn own_label(&self) -> bool {
        self.own.is_some() || self.offline
    }

    /// Строки дерева по порядку: своя подпись и папки, затем ветки с папками; свёрнутые группы — без папок.
    fn rows(&self) -> Vec<Row<'_>> {
        let mut rows = Vec::new();
        let own_label = self.own_label();
        if own_label {
            rows.push(Row::Own);
        }
        // Без подписи свернуть свою группу нечем — её папки видны всегда.
        if !own_label || !self.collapsed.contains(OWN) {
            let own: Vec<&MailboxRow> = self.of(None).collect();
            rows.extend(tree(&own, &self.folded).into_iter().map(|(m, n)| Row::Folder(m, false, n)));
        }
        for a in ordered(&self.accounts) {
            let Some(a) = self.accounts.iter().find(|x| x.id == a.id) else { continue };
            rows.push(Row::Branch(a));
            if !self.collapsed.contains(&a.id) {
                let theirs: Vec<&MailboxRow> = self.of(Some(&a.id)).collect();
                rows.extend(tree(&theirs, &self.folded).into_iter().map(|(m, n)| Row::Folder(m, true, n)));
            }
        }
        rows
    }

    /// Строка открытой папки, а если её группа свёрнута — подписи группы.
    fn current_row(&self) -> Option<usize> {
        let id = self.current.as_deref()?;
        let group = account_of(id).unwrap_or(OWN);
        self.rows().iter().position(|r| match r {
            Row::Folder(m, ..) => m.id == id,
            Row::Own => group == OWN && self.collapsed.contains(OWN),
            Row::Branch(a) => group == a.id && self.collapsed.contains(&a.id),
        })
    }

    /// Открытая папка — в свёрнутой группе `account` (`OWN` — своей): подпись группы показана выбранной.
    fn holds_current(&self, account: &str) -> bool {
        self.results.is_none()
            && self.collapsed.contains(account)
            && self.current.as_deref().is_some_and(|id| account_of(id).unwrap_or(OWN) == account)
    }

    fn folder(&self, m: &MailboxRow, indent: bool, node: Node, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.results.is_none() && self.current.as_deref() == Some(&m.id);
        let id = m.id.clone();
        let menu = m.id.clone();
        let fold = m.id.clone();
        // Шеврон подпапок (T1-04г): место под него — у каждой папки, чтобы значки уровня стояли ровно.
        let open = !self.folded.contains(&m.id);
        let chevron = div().id(SharedString::from(format!("mb-fold-{}", m.id))).w(px(12.)).flex_none().text_color(l.text3()).when(node.parent, |d| {
            d.child(if open { "▾" } else { "▸" }).cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.fold(&fold, cx);
            }))
        });
        div()
            .id(SharedString::from(format!("mb-{}", m.id)))
            .h(px(row_h(l)))
            .flex_none()
            // Отступ ветки и 16 px на уровень вложенности.
            .pl(px(if indent { 22. } else { 4. } + 16. * node.depth as f32))
            .pr(px(8.))
            .rounded(l.px(Metric::RadiusControl))
            // Как строка списка (T1-D): выбранная — тон акцента в фокусе, нейтральная без; наведение — тон акцента.
            .when(selected, |d| d.bg(l.row_fill(self.focused)).font_weight(FontWeight::SEMIBOLD))
            .when(!selected, |d| d.hover(|d| d.bg(l.row_hover())))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(chevron)
            .child(crate::chrome::icon(folder_glyph(m.role.as_deref()), 16.).text_color(l.text2()).ml(px(-6.)))
            .child(div().flex_1().min_w_0().truncate().child(m.name.clone()))
            // Непрочитанные — с сервера (`Mailbox.unreadEmails`), а не по загруженной части папки.
            .when(m.unread > 0, |d| unread(d, m.unread, l))
            // Воронка — у открытой папки.
            .when_some(self.filters.as_ref().filter(|_| selected), |d, f| d.child(crate::filters::funnel(f, f.read(cx), l)))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(FolderEvent::Open(id.clone()))))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    cx.emit(FolderEvent::Menu { id: menu.clone(), at: ev.position });
                }),
            )
            .into_any_element()
    }

    /// Заголовок ветки общего ящика: стрелка, имя; свёрнута — непрочитанные всего ящика.
    fn branch(&self, a: &AccountRow, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let open = !self.collapsed.contains(&a.id);
        let total: u32 = if open { 0 } else { self.of(Some(&a.id)).map(|m| m.unread).sum() };
        let id = a.id.clone();
        let hint = crate::shared::rights_hint(self.read_only(a), self.of(Some(&a.id)).map(|m| &m.rights));
        let own = postman_cache::profile_of(Some(&a.id)) == Some(a.id.as_str());
        let selected = self.holds_current(&a.id);
        div()
            .id(SharedString::from(format!("acc-{}", a.id)))
            .mt(px(10.))
            .h(px(28.))
            .flex_none()
            .px(px(8.))
            .rounded(l.px(Metric::RadiusControl))
            .when(selected, |d| d.bg(l.c(Role::FillCursor)).font_weight(FontWeight::SEMIBOLD))
            .when(!selected, |d| d.hover(|d| d.bg(l.hover())))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_1()
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .child(chevron(open))
            // Значок «общий» и права в подсказке (S5-28, макет `SharedMailboxes`); другая моя учётная запись (S7-03б) —
            // значок почты.
            .child(crate::chrome::icon(if own { crate::chrome::glyph::MAIL } else { crate::chrome::glyph::PEOPLE }, 14.))
            .child(div().flex_1().min_w_0().truncate().child(a.name.clone()))
            .when(self.read_only(a), |d| d.child(div().flex_none().text_size(px(11.)).child(tr!("shared-read-only"))))
            .when(total > 0, |d| unread(d, total, l))
            // Подсказка «Общий ящик · доступ» — только у общих ящиков (S7-09б).
            .when(!own, |d| d.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(&id, cx)))
            .into_any_element()
    }

    /// Подпись своей учётной записи (T1-06): шеврон, логин, без связи — метка «нет связи»; щелчок сворачивает её
    /// папки, свёрнута — непрочитанные всех её папок.
    fn own_header(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let open = !self.collapsed.contains(OWN);
        let total: u32 = if open { 0 } else { self.of(None).map(|m| m.unread).sum() };
        let selected = self.holds_current(OWN);
        let mark = self.offline.then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(18.))
                .px(px(6.))
                .rounded(px(4.))
                .border_1()
                .border_color(l.stroke())
                .text_size(px(11.))
                .child(div().size(px(6.)).rounded_full().bg(l.text3()))
                .child(tr!("states-account-offline"))
        });
        div()
            .id("own-account")
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px(px(8.))
            .pt(px(6.))
            .pb(px(4.))
            .rounded(l.px(Metric::RadiusControl))
            .when(selected, |d| d.bg(l.c(Role::FillCursor)).font_weight(FontWeight::SEMIBOLD))
            .when(!selected, |d| d.hover(|d| d.bg(l.hover())))
            .cursor_pointer()
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .child(chevron(open))
            .child(div().flex_1().min_w_0().truncate().child(self.own.clone().unwrap_or_default()))
            .children(mark)
            .when(total > 0, |d| unread(d, total, l))
            .when(self.offline, |d| {
                d.tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("states-account-offline-hint")).build(window, cx))
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle(OWN, cx)))
            .into_any_element()
    }
}

/// Шеврон группы: развёрнута — вниз, свёрнута — вправо.
fn chevron(open: bool) -> Div {
    div().w(px(12.)).flex_none().child(if open { "▾" } else { "▸" })
}

/// Ветки дерева (S7-03б): общие ящики основной учётной записи, затем каждая дополнительная — сама и её общие ящики;
/// внутри — по имени.
pub(crate) fn ordered(accounts: &[AccountRow]) -> Vec<AccountRow> {
    let mut list = accounts.to_vec();
    list.sort_by(|a, b| {
        let key = |r: &AccountRow| {
            let profile = postman_cache::profile_of(Some(&r.id)).map(str::to_owned);
            let own = profile.as_deref() == Some(r.id.as_str());
            (profile, !own, r.name.to_lowercase())
        };
        key(a).cmp(&key(b))
    });
    list
}

/// Высота строки папки: 32 px при обычной плотности (макет), на 4 px меньше — при компактной.
fn row_h(l: &Look) -> f32 {
    l.m(Metric::HRow) + 8.
}

/// Значок папки по роли JMAP.
pub(crate) fn folder_glyph(role: Option<&str>) -> char {
    use crate::chrome::glyph;
    match role {
        Some("inbox") => glyph::MAIL,
        Some("sent") => glyph::SEND,
        Some("drafts") => glyph::DRAFT,
        Some("trash") => glyph::DELETE,
        Some("archive") => glyph::ARCHIVE,
        Some("junk") => glyph::JUNK,
        _ => glyph::FOLDER,
    }
}

fn unread(d: Stateful<Div>, n: u32, l: &Look) -> Stateful<Div> {
    d.child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(l.accent_text()).child(n.to_string()))
}

impl Render for Folders {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = look(cx);
        // Режим фильтров: белая карточка с фильтрами на месте дерева.
        if let Some(f) = self.filters.as_ref().filter(|f| f.read(cx).panel) {
            return div().size_full().pt(px(4.)).pr(px(8.)).text_color(l.text()).child(crate::filters::panel(
                f,
                f.read(cx),
                crate::features::available("mail.categorize", crate::features::get(cx)),
                &l,
            ));
        }
        let mut col = div().flex().flex_col().size_full().pt(px(4.)).pr(px(8.)).pl(px(4.)).text_color(l.text());
        if let Some(n) = self.results {
            col = col.child(
                div()
                    .mx(px(4.))
                    .mb(px(4.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(l.px(Metric::RadiusControl))
                    .bg(l.picked())
                    .child(tr!("folders-results", n = n)),
            );
        }
        let rows: Vec<AnyElement> = self
            .rows()
            .into_iter()
            .map(|r| match r {
                // Своя учётная запись — подписью над папками, без связи — с меткой «нет связи» (макет «Состояния»).
                Row::Own => self.own_header(&l, cx),
                Row::Branch(a) => self.branch(a, &l, cx),
                Row::Folder(m, indent, node) => self.folder(m, indent, node, &l, cx),
            })
            .collect();
        let list = div().id("folders").flex().flex_col().gap(px(2.)).size_full().overflow_y_scroll().track_scroll(&self.scroll).children(rows);
        let list = div().relative().flex_1().min_h_0().child(list).child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Hover));
        col.child(list)
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{Folders, OWN, Row, tree};
    use postman_cache::{AccountRow, MailboxRow, Rights};
    use std::collections::HashSet;

    /// Строки дерева коротко: `@` — своя подпись, `+id` — ветка, иначе id папки.
    fn shown(f: &Folders) -> Vec<String> {
        f.rows()
            .iter()
            .map(|r| match r {
                Row::Own => "@".to_owned(),
                Row::Branch(a) => format!("+{}", a.id),
                Row::Folder(m, ..) => m.id.clone(),
            })
            .collect()
    }

    fn mb(id: &str, rights: Rights) -> MailboxRow {
        MailboxRow { id: id.into(), parent_id: None, name: id.into(), role: None, sort_order: 0, total: 0, unread: 0, rights }
    }

    fn acc(id: &str, read_only: bool) -> AccountRow {
        AccountRow { id: id.into(), name: id.into(), is_personal: false, read_only }
    }

    /// Дерево по `parent_id` (T1-04г): подпапки под родителем по `sort_order`, глубина, сирота — наверху, цикл не
    /// зацикливает; свёрнутая папка — без подпапок; переход к вложенной папке разворачивает родителей.
    #[test]
    fn folder_tree_by_parent() {
        let m =
            |id: &str, parent: Option<&str>, order: u32| MailboxRow { parent_id: parent.map(Into::into), sort_order: order, ..mb(id, Rights::ALL) };
        let all = [
            m("b", None, 2),
            m("a", None, 1),
            m("a2", Some("a"), 2),
            m("a1", Some("a"), 1),
            m("a11", Some("a1"), 0),
            m("orphan", Some("gone"), 3),
            m("x", Some("y"), 4),
            m("y", Some("x"), 5),
        ];
        let refs: Vec<&MailboxRow> = all.iter().collect();
        let got = |folded: &HashSet<String>| tree(&refs, folded).into_iter().map(|(m, n)| (m.id.as_str(), n.depth, n.parent)).collect::<Vec<_>>();
        assert_eq!(
            got(&HashSet::new()),
            [
                ("a", 0, true),
                ("a1", 1, true),
                ("a11", 2, false),
                ("a2", 1, false),
                ("b", 0, false),
                ("orphan", 0, false),
                ("x", 0, true),
                ("y", 1, true)
            ]
        );
        let folded: HashSet<String> = ["a".to_string()].into();
        assert_eq!(got(&folded).iter().map(|r| r.0).collect::<Vec<_>>(), ["a", "b", "orphan", "x", "y"]);
        let mut f = Folders::new(all.to_vec(), Vec::new(), None);
        f.folded = ["a".to_string(), "a1".to_string()].into();
        f.open("a11".into());
        assert!(f.folded.is_empty(), "родители открытой папки развёрнуты");
        assert_eq!(shown(&f), ["a", "a1", "a11", "a2", "b", "orphan", "x", "y"]);
    }

    /// Ветки дерева (S4-06): папки по аккаунтам; «только чтение» — по сессии или по правам всех папок.
    #[test]
    fn folders_are_grouped_by_account() {
        let ro = Rights { read: true, add: false, remove: false, seen: false, keywords: false, submit: false };
        let f = Folders::new(
            vec![mb("in", Rights::ALL), mb("i/in", Rights::ALL), mb("j/in", ro), mb("sent", Rights::ALL)],
            vec![acc("i", false), acc("j", false), acc("k", true)],
            None,
        );
        let ids = |a: Option<&str>| f.of(a).map(|m| m.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids(None), ["in", "sent"]);
        assert_eq!(ids(Some("i")), ["i/in"]);
        assert!(!f.read_only(&acc("i", false)));
        assert!(f.read_only(&acc("j", false)), "все папки только для чтения");
        assert!(f.read_only(&acc("k", true)), "так сказала сессия");
        assert!(!f.read_only(&acc("k", false)), "папок ещё нет — не «только чтение»");
    }

    /// T1-06: своя группа и доп. учётка сворачиваются; открытая папка свёрнутой группы — на подписи группы;
    /// переход к папке разворачивает её группу. T1-07: строка открытой папки — цель прокрутки.
    #[test]
    fn groups_collapse_and_open_expands() {
        let mut f = Folders::new(
            vec![mb("in", Rights::ALL), mb("sent", Rights::ALL), mb("i/in", Rights::ALL), mb("i/sent", Rights::ALL)],
            vec![acc("i", false)],
            Some("sent".into()),
        );
        // Без подписи своей учётки (демо) свернуть её нечем.
        f.flip(OWN);
        assert_eq!(shown(&f), ["in", "sent", "+i", "i/in", "i/sent"]);
        f.flip(OWN);
        f.own = Some("me@corp".into());
        assert_eq!(shown(&f), ["@", "in", "sent", "+i", "i/in", "i/sent"]);
        assert_eq!(f.current_row(), Some(2));
        assert!(!f.holds_current(OWN));

        f.flip(OWN);
        f.flip("i");
        assert_eq!(shown(&f), ["@", "+i"]);
        assert!(f.holds_current(OWN), "открытая папка — в свёрнутой своей группе");
        assert_eq!(f.current_row(), Some(0));
        assert!(!f.holds_current("i"));

        // Переход к папке свёрнутой доп. учётки разворачивает её, своя остаётся свёрнутой.
        f.open("i/sent".into());
        assert_eq!(shown(&f), ["@", "+i", "i/in", "i/sent"]);
        assert_eq!(f.current_row(), Some(3));
        f.open("in".into());
        assert_eq!(shown(&f), ["@", "in", "sent", "+i", "i/in", "i/sent"]);
        assert_eq!(f.current_row(), Some(1));
    }
}
