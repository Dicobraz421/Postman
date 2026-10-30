//! Панель папок (разделы — рейл модулей, `chrome.rs`). Общие ящики (S4-06) — свёртываемые ветки
//! с именем аккаунта под своими папками; своя учётная запись сворачивается так же (T1-06). Воронка у открытой папки
//! переключает панель на фильтры (S5-03, `filters.rs`). Меню папки (ПКМ, T1-04д) — «Новая папка», «Переименовать»: имя
//! — полем в строке дерева (Enter — сохранить, Esc — отмена); перенос — перетаскиванием на папку или на имя учётки.
//! Ролевые папки не переименовываются и не переносятся. «Вид беседы» — в меню письма (T1-14).
//! Дерево прокручивается с полосой при наведении, открытая папка — в видимой области (T1-07).

use std::collections::HashSet;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::{AccountRow, MailboxRow, account_of};
use postman_core::sync::Command;

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
    /// Новая папка или новое имя — полем в строке дерева (T1-04д).
    edit: Option<Edit>,
    /// Меню папки (ПКМ, T1-04д).
    menu: Option<FolderMenu>,
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
    /// Подпись группы «На этом компьютере» (S8-15).
    Local,
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
    /// Создать, переименовать или перенести папку (T1-04д) — команда синхронизации (`Command` не копируется: экран
    /// забирает её из ячейки).
    Command(std::cell::RefCell<Option<Command>>),
    /// Подсказка тостом (ролевую папку не изменить).
    Toast(String),
    /// Правка папки «На этом компьютере» (S8-15) — в базе, без сервера.
    Local(crate::local::FolderEdit),
    /// Письма брошены на папку (S8-15).
    Letters(crate::local::DraggedLetters, String),
}

/// Что правится полем в дереве.
#[derive(Clone, Debug, PartialEq)]
enum EditKind {
    /// Новая папка в аккаунте (`None` — свой) под `parent` (`None` — верхний уровень).
    New {
        account: Option<String>,
        parent: Option<String>,
    },
    Rename(String),
}

struct Edit {
    what: EditKind,
    input: Entity<InputState>,
    _sub: Subscription,
}

/// Пункт меню папки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuItem {
    New,
    Rename,
    /// Удалить пустую папку «На этом компьютере» (S8-15).
    Delete,
}

/// Пункты меню папки: у локальной — ещё «Удалить».
fn menu_items(id: &str) -> &'static [MenuItem] {
    if crate::local::is_local(id) || id == crate::local::LOCAL {
        &[MenuItem::New, MenuItem::Rename, MenuItem::Delete]
    } else {
        &[MenuItem::New, MenuItem::Rename]
    }
}

struct FolderMenu {
    id: String,
    at: Point<Pixels>,
    cursor: usize,
    focus: FocusHandle,
    focus_pending: bool,
}

/// Перетаскиваемая папка (T1-04д) и её вид под курсором.
#[derive(Clone)]
pub(crate) struct DraggedFolder {
    id: String,
    name: String,
}

impl Render for DraggedFolder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = look(cx);
        div()
            .px(px(10.))
            .py(px(4.))
            .rounded(l.px(Metric::RadiusControl))
            .bg(l.c(Role::LayerOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .shadow_md()
            .text_color(l.text())
            .child(self.name.clone())
    }
}

/// Папку можно переименовать и перенести: не ролевая (Входящие, Отправленные, …).
pub(crate) fn changeable(m: &MailboxRow) -> bool {
    m.role.is_none()
}

/// Новая папка: имя без пробелов по краям, пустое — нет команды.
pub(crate) fn new_folder(account: Option<&str>, parent: Option<&str>, name: &str) -> Option<Command> {
    let name = name.trim();
    (!name.is_empty()).then(|| Command::CreateFolder {
        account: account.map(str::to_owned),
        parent: parent.map(str::to_owned),
        name: name.to_owned(),
    })
}

/// Новая папка «На этом компьютере» (S8-15): под локальной папкой `parent` (`None` — в корне группы).
pub(crate) fn new_local(parent: Option<&str>, name: &str) -> Option<crate::local::FolderEdit> {
    let name = name.trim();
    (!name.is_empty()).then(|| crate::local::FolderEdit::Create { parent: parent.and_then(crate::local::local_folder), name: name.to_owned() })
}

/// Новое имя папки: ролевую — нельзя, то же имя или пустое — нет команды.
pub(crate) fn rename(m: &MailboxRow, name: &str) -> Option<Command> {
    let name = name.trim();
    (changeable(m) && !name.is_empty() && name != m.name).then(|| Command::RenameFolder { mailbox: m.id.clone(), name: name.to_owned() })
}

/// Новое имя локальной папки (S8-15): то же или пустое — нет правки.
pub(crate) fn rename_local(m: &MailboxRow, name: &str) -> Option<crate::local::FolderEdit> {
    let name = name.trim();
    let id = crate::local::local_folder(&m.id)?;
    (!name.is_empty() && name != m.name).then(|| crate::local::FolderEdit::Rename { id, name: name.to_owned() })
}

/// Куда бросили папку: на другую папку или на имя учётки (`Account` — верхний уровень, `None` — своя).
#[derive(Clone, Copy, Debug)]
pub(crate) enum DropTarget<'a> {
    Folder(&'a MailboxRow),
    Account(Option<&'a str>),
}

/// Перенос перетаскиванием: ролевую — нельзя; в саму себя, к прежнему родителю и в другую учётку — нет команды
/// (другую учётку и подпапку ядро отклонит и само — `sync::admit`).
pub(crate) fn drop_on(dragged: &MailboxRow, target: DropTarget<'_>) -> Option<Command> {
    if !changeable(dragged) || crate::local::is_local(&dragged.id) {
        return None;
    }
    let account = account_of(&dragged.id);
    let parent = match target {
        DropTarget::Folder(t) if t.id != dragged.id && account_of(&t.id) == account => Some(t.id.clone()),
        DropTarget::Account(a) if a == account => None,
        _ => return None,
    };
    (dragged.parent_id != parent).then(|| Command::MoveFolder { mailbox: dragged.id.clone(), parent })
}

/// Перенос локальной папки (S8-15): в другую локальную или в корень группы; к прежнему родителю и в себя — нет
/// правки (в свою подпапку база не даст).
pub(crate) fn drop_local(dragged: &MailboxRow, target: DropTarget<'_>) -> Option<crate::local::FolderEdit> {
    let id = crate::local::local_folder(&dragged.id)?;
    let parent = match target {
        DropTarget::Folder(t) if t.id != dragged.id => Some(crate::local::local_folder(&t.id)?),
        DropTarget::Account(Some(crate::local::LOCAL)) => None,
        _ => return None,
    };
    (dragged.parent_id.as_deref().and_then(crate::local::local_folder) != parent).then_some(crate::local::FolderEdit::Move { id, parent })
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
            edit: None,
            menu: None,
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

    /// ПКМ по папке: её меню у точки `at`.
    fn open_menu(&mut self, id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some(FolderMenu { id, at, cursor: 0, focus: cx.focus_handle(), focus_pending: true });
        cx.notify();
    }

    /// Снимок `folder-menu`: меню у папки `id`.
    pub(crate) fn screen_menu(&mut self, id: &str, cx: &mut Context<Self>) {
        self.open_menu(id.to_owned(), point(px(160.), px(240.)), cx);
    }

    /// Снимок `folders` (S8-15): дерево — до группы «На этом компьютере», меню её папки `id` у строки.
    pub(crate) fn screen_local_menu(&mut self, id: &str, cx: &mut Context<Self>) {
        self.open_menu(id.to_owned(), point(px(300.), px(660.)), cx);
    }

    /// Снимок `folders`: дерево прокручено к концу (группа «На этом компьютере»); `true` — уже там.
    pub(crate) fn screen_scroll_end(&mut self, cx: &mut Context<Self>) -> bool {
        if self.scroll.offset().y < px(0.) {
            return true;
        }
        if let Some(last) = self.rows().len().checked_sub(1) {
            self.scroll.scroll_to_item(last);
            cx.notify();
        }
        false
    }

    pub(crate) fn scrolled(&self) -> bool {
        self.scroll.offset().y < px(0.)
    }

    pub(crate) fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Esc: меню, затем поле правки. `false` — закрывать было нечего.
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.menu.take().is_some() || self.edit.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    fn choose(&mut self, it: MenuItem, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.menu.take() else { return };
        match it {
            // Подпись группы «На этом компьютере» — новая папка в её корне.
            MenuItem::New if m.id == crate::local::LOCAL => self.start(EditKind::New { account: Some(m.id), parent: None }, window, cx),
            MenuItem::New => {
                let account = account_of(&m.id).map(str::to_owned);
                self.start(EditKind::New { account, parent: Some(m.id) }, window, cx);
            }
            MenuItem::Rename => self.start_rename(&m.id, window, cx),
            MenuItem::Delete => {
                if let Some(id) = crate::local::local_folder(&m.id) {
                    cx.emit(FolderEvent::Local(crate::local::FolderEdit::Delete(id)));
                }
            }
        }
        cx.notify();
    }

    /// `folder.new`: новая папка в открытой (или верхнего уровня своего ящика).
    pub(crate) fn new_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self.current.clone();
        let account = parent.as_deref().and_then(account_of).map(str::to_owned);
        self.start(EditKind::New { account, parent }, window, cx);
    }

    /// `folder.rename`: открытая папка; ролевая — подсказка тостом.
    pub(crate) fn rename_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.current.clone() {
            self.start_rename(&id, window, cx);
        }
    }

    fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.mailboxes.iter().find(|m| m.id == id) {
            Some(m) if changeable(m) => self.start(EditKind::Rename(id.to_owned()), window, cx),
            Some(_) => cx.emit(FolderEvent::Toast(tr!("folder-role-fixed"))),
            None => {}
        }
    }

    fn start(&mut self, what: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        let value = match &what {
            EditKind::Rename(id) => self.mailboxes.iter().find(|m| &m.id == id).map(|m| m.name.clone()).unwrap_or_default(),
            EditKind::New { parent, .. } => {
                // Родитель — развернуть: поле — под ним.
                if let Some(p) = parent {
                    self.folded.remove(p);
                }
                String::new()
            }
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value).placeholder(tr!("folder-new-placeholder")));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe(&input, |this: &mut Self, state, ev: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let text = state.read(cx).value().to_string();
                this.commit(&text, cx);
            }
        });
        self.edit = Some(Edit { what, input, _sub: sub });
        cx.notify();
    }

    /// Enter в поле: команда серверу (дерево перечитается по `MailboxesChanged`), поле закрыто.
    fn commit(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(e) = self.edit.take() else { return };
        // «На этом компьютере» (S8-15) — правкой базы, без сервера.
        let local = match &e.what {
            EditKind::New { account: Some(a), parent } if a == crate::local::LOCAL => Some(new_local(parent.as_deref(), text)),
            EditKind::Rename(id) if crate::local::is_local(id) => {
                Some(self.mailboxes.iter().find(|m| &m.id == id).and_then(|m| rename_local(m, text)))
            }
            _ => None,
        };
        if let Some(edit) = local {
            if let Some(edit) = edit {
                cx.emit(FolderEvent::Local(edit));
            }
            cx.notify();
            return;
        }
        let command = match &e.what {
            EditKind::New { account, parent } => new_folder(account.as_deref(), parent.as_deref(), text),
            EditKind::Rename(id) => self.mailboxes.iter().find(|m| &m.id == id).and_then(|m| rename(m, text)),
        };
        if let Some(c) = command {
            cx.emit(FolderEvent::Command(Some(c).into()));
        }
        cx.notify();
    }

    /// Папку бросили на `target`.
    fn dropped(&mut self, dragged: &DraggedFolder, target: DropTarget<'_>, cx: &mut Context<Self>) {
        let Some(m) = self.mailboxes.iter().find(|m| m.id == dragged.id) else { return };
        if let Some(e) = drop_local(m, target) {
            cx.emit(FolderEvent::Local(e));
        } else if let Some(c) = drop_on(m, target) {
            cx.emit(FolderEvent::Command(Some(c).into()));
        }
    }

    /// Поле правки (новая папка — под родителем, новое имя — на месте строки).
    fn edit_row(&self, depth: usize, indent: bool, l: &Look) -> Option<AnyElement> {
        let e = self.edit.as_ref()?;
        Some(
            div()
                .h(px(row_h(l)))
                .flex_none()
                .flex()
                .items_center()
                .pl(px(if indent { 22. } else { 4. } + 16. * depth as f32 + 22.))
                .pr(px(4.))
                .child(div().flex_1().child(Input::new(&e.input)))
                .into_any_element(),
        )
    }

    /// Меню папки поверх экрана: тот же слой, что у меню письма.
    fn menu_layer(&mut self, window: &mut Window, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.menu.as_mut()?;
        if std::mem::take(&mut m.focus_pending) {
            window.focus(&m.focus, cx);
        }
        let m = self.menu.as_ref()?;
        let fixed = m.id == crate::local::LOCAL || self.mailboxes.iter().find(|f| f.id == m.id).is_some_and(|f| !changeable(f));
        let items = menu_items(&m.id).iter().enumerate().map(|(k, &it)| {
            let off = matches!(it, MenuItem::Rename | MenuItem::Delete) && fixed;
            let (glyph, key) = match it {
                MenuItem::New => (crate::chrome::glyph::FOLDER, "folder-menu-new"),
                MenuItem::Rename => (crate::chrome::glyph::EDIT, "folder-menu-rename"),
                MenuItem::Delete => (crate::chrome::glyph::DELETE, "local-delete"),
            };
            crate::bulk::menu_line(("folder-menu-item", k), l)
                .when(m.cursor == k && !off, |d| d.bg(l.hover()))
                .when(off, |d| d.text_color(l.c(Role::TextDisabled)).cursor_default())
                .child(crate::chrome::icon(glyph, 16.).text_color(if off { l.c(Role::TextDisabled) } else { l.text2() }))
                .child(div().flex_1().child(tr!(key)))
                .when(!off, |d| d.on_click(cx.listener(move |this, _, window, cx| this.choose(it, window, cx))))
        });
        let body = div()
            .id("folder-menu")
            .key_context("ContextMenu")
            .track_focus(&m.focus)
            .occlude()
            .w(px(220.))
            .p(px(4.))
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .shadow_lg()
            .text_size(px(13.))
            .text_color(l.text())
            .children(items)
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                let Some(m) = &mut this.menu else { return };
                let items = menu_items(&m.id);
                match ev.keystroke.key.as_str() {
                    "down" => m.cursor = (m.cursor + 1) % items.len(),
                    "up" => m.cursor = (m.cursor + items.len() - 1) % items.len(),
                    "enter" | "space" => {
                        let it = items[m.cursor % items.len()];
                        if !(matches!(it, MenuItem::Rename | MenuItem::Delete) && fixed) {
                            this.choose(it, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }));
        Some(deferred(anchored().position(m.at).snap_to_window_with_margin(px(8.)).child(body)).with_priority(2).into_any_element())
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
        // «На этом компьютере» (S8-15) — последней группой, пока в ней есть папки.
        let local: Vec<&MailboxRow> = self.of(Some(crate::local::LOCAL)).collect();
        if !local.is_empty() {
            rows.push(Row::Local);
            if !self.collapsed.contains(crate::local::LOCAL) {
                rows.extend(tree(&local, &self.folded).into_iter().map(|(m, n)| Row::Folder(m, true, n)));
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
            Row::Local => group == crate::local::LOCAL && self.collapsed.contains(crate::local::LOCAL),
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
        let fold = m.id.clone();
        let menu = m.id.clone();
        let drop = m.id.clone();
        let letters_to = m.id.clone();
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
            // «На этом компьютере» (S8-15, макет): число писем — серым.
            .when(crate::local::is_local(&m.id) && m.total > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(l.px(Metric::TypeCaption))
                        .text_color(l.text3())
                        .child(crate::migration::count_text(u64::from(m.total))),
                )
            })
            // Воронка — у открытой папки.
            .when_some(self.filters.as_ref().filter(|_| selected), |d, f| d.child(crate::filters::funnel(f, f.read(cx), l)))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(FolderEvent::Open(id.clone()))))
            // Меню папки (T1-04д): «Новая папка», «Переименовать»; «Вид беседы» — в меню письма (T1-14).
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_menu(menu.clone(), ev.position, cx);
                }),
            )
            // Перенос (T1-04д): ролевую не тащить; бросить можно на любую папку своей учётки.
            .when(changeable(m), |d| d.on_drag(DraggedFolder { id: m.id.clone(), name: m.name.clone() }, |f, _, _, cx| cx.new(|_| f.clone())))
            .drag_over::<DraggedFolder>(|s, _, _, _| s.bg(crate::roles::now().row_hover()))
            .on_drop(cx.listener(move |this, d: &DraggedFolder, _, cx| {
                let target = this.mailboxes.iter().find(|x| x.id == drop).cloned();
                if let Some(t) = target {
                    this.dropped(d, DropTarget::Folder(&t), cx);
                }
            }))
            // Письма на папку (S8-15): в другую учётку и «На этом компьютере» — перенос ядром.
            .drag_over::<crate::local::DraggedLetters>(|s, _, _, _| s.bg(crate::roles::now().row_hover()))
            .on_drop(cx.listener(move |_, d: &crate::local::DraggedLetters, _, cx| cx.emit(FolderEvent::Letters(d.clone(), letters_to.clone()))))
            .into_any_element()
    }

    /// Подпись группы «На этом компьютере» (S8-15, макет S8-D п. 4): шеврон, значок, имя; подсказка — где хранится
    /// и сколько писем; меню — «Новая папка»; бросок локальной папки — в корень группы.
    fn local_header(&self, l: &Look, cx: &mut Context<Self>) -> AnyElement {
        use crate::local::LOCAL;
        let open = !self.collapsed.contains(LOCAL);
        let n: u64 = self.of(Some(LOCAL)).map(|m| u64::from(m.total)).sum();
        let hint = tr!("local-group-hint", n = n as i64, count = crate::migration::count_text(n));
        let selected = self.holds_current(LOCAL);
        div()
            .id("local-group")
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
            .child(crate::chrome::icon(crate::local::PC, 14.))
            .child(div().flex_1().min_w_0().truncate().child(tr!("local-group")))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.toggle(LOCAL, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_menu(LOCAL.to_owned(), ev.position, cx);
                }),
            )
            .drag_over::<DraggedFolder>(|s, _, _, _| s.bg(crate::roles::now().row_hover()))
            .on_drop(cx.listener(|this, d: &DraggedFolder, _, cx| this.dropped(d, DropTarget::Account(Some(LOCAL)), cx)))
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
            // Бросок папки на подпись — верхний уровень своего ящика (T1-04д).
            .drag_over::<DraggedFolder>(|s, _, _, _| s.bg(crate::roles::now().row_hover()))
            .on_drop(cx.listener(|this, d: &DraggedFolder, _, cx| this.dropped(d, DropTarget::Account(None), cx)))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = look(cx);
        let menu = self.menu_layer(window, &l, cx);
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
        let edit = self.edit.as_ref().map(|e| e.what.clone());
        let mut rows: Vec<AnyElement> = Vec::new();
        for r in self.rows() {
            match r {
                // Своя учётная запись — подписью над папками, без связи — с меткой «нет связи» (макет «Состояния»).
                Row::Own => rows.push(self.own_header(&l, cx)),
                Row::Branch(a) => rows.push(self.branch(a, &l, cx)),
                Row::Local => rows.push(self.local_header(&l, cx)),
                // Переименование — поле на месте строки; новая папка — поле под родителем.
                Row::Folder(m, indent, node) if edit == Some(EditKind::Rename(m.id.clone())) => rows.extend(self.edit_row(node.depth, indent, &l)),
                Row::Folder(m, indent, node) => {
                    rows.push(self.folder(m, indent, node, &l, cx));
                    if matches!(&edit, Some(EditKind::New { parent: Some(p), .. }) if *p == m.id) {
                        rows.extend(self.edit_row(node.depth + 1, indent, &l));
                    }
                }
            }
        }
        if matches!(&edit, Some(EditKind::New { parent: None, .. })) {
            rows.extend(self.edit_row(0, false, &l));
        }
        let list = div().id("folders").flex().flex_col().gap(px(2.)).size_full().overflow_y_scroll().track_scroll(&self.scroll).children(rows);
        let list = div().relative().flex_1().min_h_0().child(list).child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Hover));
        col.child(list).children(menu)
    }
}

/// Отказ сервера или ядра (`FolderFailed`) — текстом для тоста.
pub(crate) fn failed_text(error: &str) -> String {
    use postman_core::sync::{FOLDER_INTO_ITSELF, FOLDER_OTHER_ACCOUNT, FOLDER_UNSUPPORTED};
    match error {
        FOLDER_UNSUPPORTED => tr!("folder-unsupported"),
        FOLDER_INTO_ITSELF => tr!("folder-into-itself"),
        FOLDER_OTHER_ACCOUNT => tr!("folder-other-account"),
        e => tr!("folder-failed", error = e.to_owned()),
    }
}

impl crate::Mailer {
    /// Команда папки (T1-04д): серверу; в демо папки не меняются — подсказка.
    pub(crate) fn folder_command(&mut self, c: Command, cx: &mut Context<Self>) {
        if !self.backend.is_live() {
            return self.toast(pets_ui::Level::Info, tr!("folder-demo"), cx);
        }
        if !self.backend.send(c) {
            self.sync_stopped(cx);
        }
    }
}

/// Опись части (ADR 0005, `parts.rs`): команды и сцена папок (T1-04д).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "folders", commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Папки (T1-04д): новая и переименовать — без клавиш, кнопками меню и палитрой.
pub(crate) mod folder {
    gpui_kit::actions!(folder, [New, Rename]);
}

const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAIL, cmd};
    &[cmd("folder.new", &[], MAIL), cmd("folder.rename", &[], MAIL)]
};

fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &folder::New, window, cx| this.folders.update(cx, |f, cx| f.new_here(window, cx))))
        .on_action(cx.listener(|this, _: &folder::Rename, window, cx| this.folders.update(cx, |f, cx| f.rename_current(window, cx))))
}

/// Сцены `--screens` папок.
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::Scene;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Меню папки «Клиенты» (T1-04д): «Новая папка», «Переименовать».
        Scene::new("folder-menu", menu, menu_ready),
    ];

    fn menu(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let id = m.folders.read(cx).mailboxes.iter().find(|f| f.role.is_none() && f.parent_id.is_some()).map(|f| f.id.clone());
        let Some(id) = id else { return Err("no nested folder in the demo".into()) };
        m.folders.update(cx, |f, cx| f.screen_menu(&id, cx));
        Ok(())
    }

    fn menu_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.folders.read(cx).menu_open()
    }

    /// Меню папки — не дальше своей сцены.
    pub(crate) fn close(m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.folders.update(cx, |f, cx| {
            f.escape(cx);
        });
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{DropTarget, Folders, OWN, Row, changeable, drop_on, new_folder, rename, tree};
    use postman_cache::{AccountRow, MailboxRow, Rights};
    use postman_core::sync::Command;
    use std::collections::HashSet;

    /// Строки дерева коротко: `@` — своя подпись, `+id` — ветка, иначе id папки.
    fn shown(f: &Folders) -> Vec<String> {
        f.rows()
            .iter()
            .map(|r| match r {
                Row::Own => "@".to_owned(),
                Row::Branch(a) => format!("+{}", a.id),
                Row::Folder(m, ..) => m.id.clone(),
                Row::Local => "~".to_owned(),
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

    /// Команды папок (T1-04д): имя — без пробелов по краям; ролевую не переименовать и не перенести; перенос — на
    /// другую папку той же учётки или на её подпись, к прежнему родителю и в саму себя — нет.
    #[test]
    fn folder_commands() {
        let mut inbox = mb("in", Rights::ALL);
        inbox.role = Some("inbox".into());
        let a = mb("a", Rights::ALL);
        let b = MailboxRow { parent_id: Some("a".into()), ..mb("b", Rights::ALL) };
        let other = mb("@2/x", Rights::ALL);
        assert!(
            matches!(new_folder(None, Some("a"), "  New "), Some(Command::CreateFolder { account: None, parent: Some(p), name }) if p == "a" && name == "New")
        );
        assert!(new_folder(None, None, "  ").is_none());
        assert!(!changeable(&inbox) && changeable(&a));
        assert!(rename(&inbox, "x").is_none(), "ролевую — нельзя");
        assert!(rename(&a, "a").is_none(), "то же имя");
        assert!(matches!(rename(&a, "Z"), Some(Command::RenameFolder { mailbox, name }) if mailbox == "a" && name == "Z"));
        assert!(matches!(drop_on(&a, DropTarget::Folder(&b)), Some(Command::MoveFolder { mailbox, parent: Some(p) }) if mailbox == "a" && p == "b"));
        assert!(drop_on(&b, DropTarget::Folder(&a)).is_none(), "уже там");
        assert!(matches!(drop_on(&b, DropTarget::Account(None)), Some(Command::MoveFolder { parent: None, .. })));
        assert!(drop_on(&a, DropTarget::Account(None)).is_none(), "уже наверху");
        assert!(drop_on(&a, DropTarget::Folder(&a)).is_none());
        assert!(drop_on(&a, DropTarget::Folder(&other)).is_none(), "другая учётка");
        assert!(drop_on(&inbox, DropTarget::Folder(&a)).is_none(), "ролевую — нельзя");
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
