//! Панель папок (разделы — рейл модулей, `chrome.rs`). Общие ящики (S4-06) — свёртываемые ветки
//! с именем аккаунта под своими папками. Воронка у открытой папки переключает панель на фильтры (S5-03,
//! `filters.rs`). Правая кнопка по папке — её меню (`folder_menu.rs`, S5-05б).

use std::collections::HashSet;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::{AccountRow, MailboxRow, account_of};

use pets_ui::theme::{Metric, Role};

use crate::filters::FilterModel;
use crate::roles::{Look, look};

// --- папки ---

pub(crate) struct Folders {
    pub(crate) mailboxes: Vec<MailboxRow>,
    /// Общие ящики (S4-06): ветка на аккаунт.
    pub(crate) accounts: Vec<AccountRow>,
    /// Свёрнутые ветки общих ящиков (id аккаунта).
    pub(crate) collapsed: HashSet<String>,
    pub(crate) current: Option<String>,
    /// Число результатов, пока показан поиск.
    pub(crate) results: Option<usize>,
    /// Фильтры (S5-03): модель, общая со списком; панель на месте папок — `FilterModel::panel`.
    pub(crate) filters: Option<Entity<FilterModel>>,
    /// Своя учётная запись (логин) — подпись над своими папками; без неё (демо) подписи нет, пока есть связь.
    pub(crate) own: Option<String>,
    /// Связи с сервером нет (S5-08): метка «нет связи» у своего аккаунта.
    pub(crate) offline: bool,
    _watch: Option<Subscription>,
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
        Self { mailboxes, accounts, collapsed: HashSet::new(), current, results: None, filters: None, own: None, offline: false, _watch: None }
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

    fn toggle(&mut self, account: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(account) {
            self.collapsed.insert(account.to_owned());
        }
        cx.notify();
    }

    fn folder(&self, m: &MailboxRow, indent: bool, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.results.is_none() && self.current.as_deref() == Some(&m.id);
        let id = m.id.clone();
        let menu = m.id.clone();
        div()
            .id(SharedString::from(format!("mb-{}", m.id)))
            .h(px(row_h(l)))
            .flex_none()
            .pl(px(if indent { 26. } else { 8. }))
            .pr(px(8.))
            .rounded(l.px(Metric::RadiusControl))
            .when(selected, |d| d.bg(l.c(Role::FillCursor)).font_weight(FontWeight::SEMIBOLD))
            .when(!selected, |d| d.hover(|d| d.bg(l.hover())))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(crate::chrome::icon(folder_glyph(m.role.as_deref()), 16.).text_color(l.text2()))
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
        div()
            .id(SharedString::from(format!("acc-{}", a.id)))
            .mt(px(10.))
            .h(px(28.))
            .flex_none()
            .px(px(8.))
            .rounded(l.px(Metric::RadiusControl))
            .hover(|d| d.bg(l.hover()))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_1()
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .child(div().w(px(12.)).child(if open { "▾" } else { "▸" }))
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
        let mut list = div().id("folders").flex().flex_col().gap(px(2.)).flex_1().min_h_0().overflow_y_scroll();
        // Свой аккаунт — подписью над папками, без связи — с меткой «нет связи» (макет «Состояния»).
        if self.own.is_some() || self.offline {
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
            list = list.child(
                div()
                    .id("own-account")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .pt(px(6.))
                    .pb(px(4.))
                    .text_size(l.px(Metric::TypeCaption))
                    .text_color(l.text2())
                    .child(div().flex_1().min_w_0().truncate().child(self.own.clone().unwrap_or_default()))
                    .children(mark)
                    .when(self.offline, |d| {
                        d.tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("states-account-offline-hint")).build(window, cx))
                    }),
            );
        }
        let own: Vec<AnyElement> = self.of(None).map(|m| self.folder(m, false, &l, cx)).collect();
        list = list.children(own);
        for a in &ordered(&self.accounts) {
            list = list.child(self.branch(a, &l, cx));
            if !self.collapsed.contains(&a.id) {
                let folders: Vec<AnyElement> = self.of(Some(&a.id)).map(|m| self.folder(m, true, &l, cx)).collect();
                list = list.children(folders);
            }
        }
        col.child(list)
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::Folders;
    use postman_cache::{AccountRow, MailboxRow, Rights};

    fn mb(id: &str, rights: Rights) -> MailboxRow {
        MailboxRow { id: id.into(), parent_id: None, name: id.into(), role: None, sort_order: 0, total: 0, unread: 0, rights }
    }

    fn acc(id: &str, read_only: bool) -> AccountRow {
        AccountRow { id: id.into(), name: id.into(), is_personal: false, read_only }
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
}
