//! «На этом компьютере» и перенос между учётками (S8-15, ADR 0006, макет S8-D «Перенос»).
//!
//! Папки формата 6 — в дереве как [`MailboxRow`]: папки учёток — по ключам кэша, как прежде; локальные — ключом
//! `~local/<FolderId>` (`~` в id сервера не бывает), их группа — последней в дереве (`folders.rs`). Список любой
//! папки читается по `FolderId` (`Cache::folder_page`): письмо — ключом своей действующей связи с сервером, без неё
//! (перенесённое, локальное) — ссылкой `#<LetterId>` (`postman_core::store::letter_ref`); значок переноса строки —
//! [`Mark`] по месту письма.
//!
//! Папки «На этом компьютере» правит пользователь без сервера: создать, переименовать, перенести, удалить пустую —
//! прямо в базе (в фоне). Письма в папку другой учётки или «На этом компьютере» — `Command::MoveLetters` в поток
//! ядра (`migration::Core`): перенос в базе одной транзакцией, список цели — по его событию.

use std::collections::HashMap;

use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::{Cache, EmailRow, FolderId, FolderRow, LetterRow, MailboxRow, MigrationState, Owner, Place, Refused, Rights, scoped, unscoped};
use postman_core::store::{letter_ref, parse_ref};
use postman_core::sync::Command;

use crate::Mailer;

/// «Учётка» ключей папок «На этом компьютере».
pub(crate) const LOCAL: &str = "~local";

/// Значок строки «ждёт переноса» (Segoe Fluent Icons Upload).
pub(crate) const UPLOAD: char = '\u{E898}';
/// «На этом компьютере» (TVMonitor): значок группы и строки «только на этом компьютере».
pub(crate) const PC: char = '\u{E7F4}';

/// Ключ локальной папки.
pub(crate) fn key(id: FolderId) -> String {
    scoped(Some(LOCAL), &id.0.to_string())
}

/// Локальная папка ключа; ключ папки учётки — `None`.
pub(crate) fn local_folder(key: &str) -> Option<FolderId> {
    match unscoped(key) {
        (Some(LOCAL), n) => n.parse().ok().map(FolderId),
        _ => None,
    }
}

pub(crate) fn is_local(key: &str) -> bool {
    local_folder(key).is_some()
}

/// Учётка ключа (основная — пустая строка) или [`LOCAL`]: так группируются папки и решается, перенос ли это между
/// учётками.
pub(crate) fn home(key: &str) -> &str {
    unscoped(key).0.unwrap_or("")
}

/// Учётка письма по ключу кэша; ссылка `#<LetterId>` — письмо без действующей связи: его учётку знает только база.
fn letter_home(key: &str) -> Option<&str> {
    parse_ref(key).is_none().then(|| home(key))
}

/// Папки учёток и «На этом компьютере» одним списком дерева.
pub(crate) fn mailboxes(cache: &Cache) -> Vec<MailboxRow> {
    let mut rows = cache.mailboxes().unwrap_or_default();
    rows.extend(local_rows(cache));
    rows
}

/// Локальные папки — строками дерева: число писем — `total` (непрочитанных у них не считаем: сервера нет).
pub(crate) fn local_rows(cache: &Cache) -> Vec<MailboxRow> {
    let folders = cache.folders().unwrap_or_default();
    folders
        .into_iter()
        .filter(|(_, f)| f.owner == Owner::Local)
        .enumerate()
        .map(|(i, (id, f))| MailboxRow {
            id: key(id),
            parent_id: f.parent.map(key),
            name: f.name,
            role: None,
            sort_order: i as u32,
            total: cache.folder_count(id).unwrap_or(0) as u32,
            unread: 0,
            rights: Rights::ALL,
        })
        .collect()
}

/// Папка формата 6 по ключу дерева.
pub(crate) fn folder_id(cache: &Cache, key: &str) -> Option<FolderId> {
    if let Some(id) = local_folder(key) {
        return Some(id);
    }
    let (account, id) = unscoped(key);
    cache.folder_of(account.unwrap_or(""), id).ok().flatten()
}

/// Писем в папке.
pub(crate) fn count(cache: &Cache, mailbox: &str) -> usize {
    folder_id(cache, mailbox).and_then(|f| cache.folder_count(f).ok()).unwrap_or(0)
}

/// Окно списка папки: новые сверху; письма без действующей связи — тоже (ссылкой `#<LetterId>`).
pub(crate) fn page(cache: &Cache, mailbox: &str, offset: usize, limit: usize) -> Vec<EmailRow> {
    let Some(f) = folder_id(cache, mailbox) else { return Vec::new() };
    let letters = cache.folder_page(f, offset, limit).unwrap_or_default();
    let mut keys = Keys { cache, folders: HashMap::new() };
    letters.into_iter().map(|(id, row)| keys.email(id, row)).collect()
}

/// Ключи папок дерева по `FolderId` — с запоминанием на одно окно.
struct Keys<'a> {
    cache: &'a Cache,
    folders: HashMap<u64, Option<String>>,
}

impl Keys<'_> {
    fn folder(&mut self, id: FolderId) -> Option<String> {
        let cache = self.cache;
        self.folders.entry(id.0).or_insert_with(|| cache.folder(id).ok().flatten().and_then(|f| folder_key(id, &f))).clone()
    }

    fn email(&mut self, id: postman_cache::LetterId, row: LetterRow) -> EmailRow {
        let refs = self.cache.remotes(id).unwrap_or_default();
        let key = match refs.iter().find(|r| !r.detached) {
            Some(r) => scoped(Some(r.account.as_str()).filter(|a| !a.is_empty()), &r.id),
            None => letter_ref(postman_core::model::LetterId(id.0)),
        };
        let mailbox_ids = row.folders.iter().filter_map(|f| self.folder(*f)).collect();
        EmailRow {
            id: key,
            thread_id: row.thread_id,
            mailbox_ids,
            received_at: row.received_at,
            subject: row.subject,
            from_name: row.from_name,
            from_addr: row.from_addr,
            preview: row.preview,
            seen: row.seen,
            flagged: row.flagged,
            has_attachment: row.has_attachment,
            size: row.size,
            keywords: row.keywords,
        }
    }
}

/// Ключ дерева папки формата 6.
pub(crate) fn folder_key(id: FolderId, f: &FolderRow) -> Option<String> {
    match &f.owner {
        Owner::Local => Some(key(id)),
        Owner::Account(a) => f.remote.as_deref().map(|r| scoped(Some(a.as_str()).filter(|a| !a.is_empty()), r)),
    }
}

// --- значки строки ---

/// Значок переноса строки письма (макет S8-D п. 3): в первой строке перед временем, цветом подписей; в папках «На
/// этом компьютере» не ставится — там всё локально.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Ждёт переноса в учётку `account` (ключ учётки папки, основная — пустая строка); `state` — задания.
    Pending { account: String, state: MigrationState },
    /// Только на этом компьютере: ответ «Оставить локальным архивом» или отказ сервера.
    Local { account: String },
    /// Больше предела сервера учётки — осталось на этом компьютере; `size` — байт.
    TooLarge { account: String, size: u64 },
}

impl Mark {
    pub(crate) fn glyph(&self) -> char {
        match self {
            Mark::Pending { .. } => UPLOAD,
            Mark::Local { .. } | Mark::TooLarge { .. } => PC,
        }
    }

    /// Подсказка значка; `server` — имя учётки.
    pub(crate) fn hint(&self, server: &str) -> String {
        match self {
            Mark::Pending { state: MigrationState::Paused, .. } => tr!("migration-row-paused"),
            Mark::Pending { state: MigrationState::QuotaFull, .. } => tr!("migration-row-no-space", server = server),
            Mark::Pending { .. } => tr!("migration-row-pending", server = server),
            Mark::Local { .. } => tr!("migration-row-local", server = server),
            Mark::TooLarge { size, .. } => tr!("migration-row-too-large", size = crate::migration::size_text(*size), server = server),
        }
    }

    pub(crate) fn account(&self) -> &str {
        match self {
            Mark::Pending { account, .. } | Mark::Local { account } | Mark::TooLarge { account, .. } => account,
        }
    }
}

/// Значок письма по месту и папке: в папке учётки письмо без связи с её сервером — ждёт переноса, на этом
/// компьютере или слишком большое. `job` — состояние задания.
pub(crate) fn mark_of(row: &LetterRow, account: Option<&str>, job: impl FnOnce(postman_cache::JobId) -> MigrationState) -> Option<Mark> {
    let account = account?.to_owned();
    Some(match row.place {
        Place::Synced => return None,
        Place::Outbound(j) => Mark::Pending { account, state: job(j) },
        Place::Local | Place::Refused(Refused::Rejected) => Mark::Local { account },
        Place::Refused(Refused::TooLarge) => Mark::TooLarge { account, size: row.size },
    })
}

/// Значки писем окна списка: только у писем без действующей связи (`#<LetterId>`) в папке учётки.
pub(crate) fn marks(cache: &Cache, rows: &[EmailRow]) -> HashMap<String, Mark> {
    let mut jobs: HashMap<u64, MigrationState> = HashMap::new();
    let mut out = HashMap::new();
    for r in rows {
        let Some(id) = parse_ref(&r.id) else { continue };
        let Ok(Some(letter)) = cache.letter(postman_cache::LetterId(id.0)) else { continue };
        let account = r.mailbox_ids.iter().map(|m| home(m)).find(|h| *h != LOCAL);
        let mark = mark_of(&letter, account, |j| {
            *jobs.entry(j.0).or_insert_with(|| cache.job(j).ok().flatten().map_or(MigrationState::Running, |row| row.state))
        });
        if let Some(m) = mark {
            out.insert(r.id.clone(), m);
        }
    }
    out
}

// --- перенос писем ---

/// Перенос писем `ids` в папку `to` — между учётками (или в «На этом компьютере», или из неё): тогда — ядром
/// (`MoveLetters`), иначе — прежней правкой папки учётки (`Act::Move`). Письмо без связи (`#…`) — всегда ядром.
pub(crate) fn across(ids: &[String], to: &str) -> bool {
    let target = home(to);
    target == LOCAL || ids.iter().any(|id| letter_home(id) != Some(target))
}

/// Команда переноса между учётками: письма — по ключам (ключ кэша или ссылка), папка — по ключу дерева; неизвестные
/// письма пропускаются, неизвестная папка — нет команды.
pub(crate) fn move_command(cache: &Cache, ids: &[String], to: &str) -> Option<Command> {
    let to = folder_id(cache, to)?;
    let ids: Vec<_> = ids.iter().filter_map(|k| postman_core::store::letter_of_key(cache, k).ok().flatten()).collect();
    (!ids.is_empty()).then(|| Command::MoveLetters { ids, to: postman_core::model::FolderId(to.0) })
}

/// Папка в «В папку ▾» (S8-15): ключ, имя и учётка — подписью группы (`None` — учётка открытой папки).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) group: Option<String>,
}

impl Target {
    /// Одной строкой (подменю контекстного меню): «Учётка · Папка».
    pub(crate) fn label(&self) -> String {
        match &self.group {
            Some(g) => format!("{g} · {}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Папки для «В папку ▾» (S8-15): все папки всех учёток и «На этом компьютере», кроме открытой; сначала учётка
/// открытой папки, затем основная, другие учётки по порядку дерева, «На этом компьютере» — последней.
pub(crate) fn folder_items(order: &[String], names: &HashMap<String, String>, accounts: &HashMap<String, String>, open: Option<&str>) -> Vec<Target> {
    let here = open.map_or("", home);
    let mut homes: Vec<&str> = vec![here, ""];
    for id in order {
        let h = home(id);
        if !homes.contains(&h) && h != LOCAL {
            homes.push(h);
        }
    }
    homes.push(LOCAL);
    let mut out = Vec::new();
    for (k, h) in homes.iter().enumerate() {
        if k > 0 && homes[..k].contains(h) {
            continue;
        }
        let group = (*h != here).then(|| if *h == LOCAL { tr!("local-group") } else { accounts.get(*h).cloned().unwrap_or_else(|| (*h).to_owned()) });
        for id in order.iter().filter(|id| home(id) == *h && open != Some(id.as_str())) {
            out.push(Target { id: id.clone(), name: names.get(id).cloned().unwrap_or_default(), group: group.clone() });
        }
    }
    out
}

/// Перетаскиваемые письма (S8-15): строка `id`; выделенная (`picked`) несёт всё выделение (`n` писем). Брошены на
/// папку — перенос (между учётками — ядром).
#[derive(Clone, Debug)]
pub(crate) struct DraggedLetters {
    pub(crate) id: String,
    pub(crate) picked: bool,
    pub(crate) n: usize,
}

impl Render for DraggedLetters {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        div()
            .px(px(10.))
            .py(px(4.))
            .rounded(l.px(pets_ui::theme::Metric::RadiusControl))
            .bg(l.c(pets_ui::theme::Role::LayerOverlay))
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .shadow_md()
            .text_color(l.text())
            .child(tr!("local-drop", n = crate::bulk::plural_n(self.n)))
    }
}

// --- папки «На этом компьютере» ---

/// Правка локальной папки (без сервера).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FolderEdit {
    Create { parent: Option<FolderId>, name: String },
    Rename { id: FolderId, name: String },
    Move { id: FolderId, parent: Option<FolderId> },
    Delete(FolderId),
}

/// Итог правки: `false` у удаления — папка не пустая.
fn apply(cache: &Cache, e: &FolderEdit) -> Result<bool, String> {
    let r = match e {
        FolderEdit::Create { parent, name } => cache.create_folder(*parent, name).map(|_| true),
        FolderEdit::Rename { id, name } => cache.rename_folder(*id, name).map(|_| true),
        FolderEdit::Move { id, parent } => cache.move_folder(*id, *parent).map(|_| true),
        FolderEdit::Delete(id) => cache.delete_folder(*id),
    };
    r.map_err(|e| e.to_string())
}

impl Mailer {
    /// Правка папки «На этом компьютере» — в базе (в фоне), затем дерево заново.
    pub(crate) fn local_folder_edit(&mut self, e: FolderEdit, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move {
            let r = apply(&cache, &e);
            (e, r)
        });
        cx.spawn(async move |this, cx| {
            let (e, r) = task.await;
            let _ = this.update(cx, |m, cx| {
                match r {
                    Ok(false) => m.toast(pets_ui::Level::Info, tr!("local-delete-not-empty"), cx),
                    Ok(true) => {
                        if let FolderEdit::Delete(id) = e
                            && m.folders.read(cx).current.as_deref() == Some(key(id).as_str())
                        {
                            m.folders.update(cx, |f, _| f.current = None);
                        }
                    }
                    Err(err) => {
                        tracing::warn!("local folder {e:?}: {err}");
                        m.toast(pets_ui::Level::Error, tr!("folder-failed", error = err), cx);
                    }
                }
                m.reload_folders(cx);
            });
        })
        .detach();
    }

    /// Письма `ids` — в папку `to`: между учётками — ядром (`MoveLetters`), внутри учётки — прежним переносом.
    pub(crate) fn move_letters(&mut self, ids: Vec<String>, to: String, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        if !across(&ids, &to) {
            return self.run_bulk(ids, crate::bulk::Ask::Move(to), cx);
        }
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { move_command(&cache, &ids, &to) });
        cx.spawn(async move |this, cx| {
            let Some(c) = task.await else { return };
            let _ = this.update(cx, |m, cx| {
                // Открытое письмо уходит из папки: выделение — по id, список цели — после события ядра.
                m.list.update(cx, |l, cx| l.clear_selection(cx));
                m.core_send(c, cx);
            });
        })
        .detach();
    }

    /// Письма брошены на папку `to` (S8-15): выделенная строка — всё выделение.
    pub(crate) fn letters_dropped(&mut self, d: &DraggedLetters, to: String, cx: &mut Context<Self>) {
        let ids = if d.picked { self.list.read(cx).selection.ids().to_vec() } else { vec![d.id.clone()] };
        let here = self.list.read(cx).mailbox.clone();
        if here.as_deref() == Some(to.as_str()) {
            return;
        }
        self.move_letters(ids, to, cx);
    }

    /// Имя учётки по ключу (основная — пустая строка): своя — логин (в демо — адрес владельца ящика), другие — имя
    /// ветки дерева.
    pub(crate) fn account_name(&self, account: &str, cx: &App) -> String {
        if account == LOCAL {
            return tr!("local-group");
        }
        if account.is_empty() {
            return self.folders.read(cx).own.clone().unwrap_or_else(|| postman_demo::ME.to_owned());
        }
        let profile = postman_cache::profile_of(Some(account)).unwrap_or(account);
        self.folders
            .read(cx)
            .accounts
            .iter()
            .find(|a| a.id == account || a.id == profile)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| account.to_owned())
    }

    /// Имена учёток — списку (подсказки значков, «В папку ▾»).
    pub(crate) fn sync_servers(&mut self, cx: &mut Context<Self>) {
        let servers = self.account_names(cx);
        self.list.update(cx, |l, cx| {
            l.servers = servers;
            cx.notify();
        });
    }

    /// Имена учёток для списка (подсказки значков, «В папку ▾»): ключ → имя.
    pub(crate) fn account_names(&self, cx: &App) -> HashMap<String, String> {
        let mut names: HashMap<String, String> = HashMap::new();
        for m in &self.folders.read(cx).mailboxes {
            let h = home(&m.id);
            if !names.contains_key(h) {
                names.insert(h.to_owned(), self.account_name(h, cx));
            }
        }
        names
    }
}

/// Опись части (ADR 0005, `parts.rs`): тексты и сцены «На этом компьютере» и переноса между учётками.
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "local", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/local.ftl")), ("en", include_str!("../i18n/en/local.ftl"))];

/// Сцены `--screens`: дерево с группой «На этом компьютере», открытая локальная папка, «В папку ▾» по всем учёткам.
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::Scene;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Дерево (S8-15): учётки, затем группа «На этом компьютере» с «Архив 2024» и «Проекты»; меню локальной папки.
        Scene::new("folders", folders, folders_ready).wait(folders_wait),
        // Открыта «На этом компьютере · Архив 2024»: 1 200 писем только в базе, без значков.
        Scene::new("local-folders", local_folders, local_ready),
        // Выделены три письма «Входящих» Exchange, открыт «В папку ▾»: папки всех учёток и «На этом компьютере».
        Scene::new("move-across", move_across, move_ready).wait(move_wait),
    ];

    fn local_key(m: &Mailer, cx: &App, name: &str) -> Option<String> {
        m.folders.read(cx).mailboxes.iter().find(|f| super::is_local(&f.id) && f.name == name).map(|f| f.id.clone())
    }

    fn folders(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let id = local_key(m, cx, postman_demo::store::ARCHIVE).ok_or("no local folder in the demo")?;
        m.folders.update(cx, |f, cx| f.screen_local_menu(&id, cx));
        Ok(())
    }

    fn folders_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let f = m.folders.read(cx);
        f.menu_open() && f.scrolled()
    }

    fn folders_wait(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.folders.update(cx, |f, cx| {
            f.screen_scroll_end(cx);
        });
    }

    fn local_folders(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        crate::folders::scenes::close(m, cx);
        let id = local_key(m, cx, postman_demo::store::ARCHIVE).ok_or("no local folder in the demo")?;
        m.open_mailbox(id, cx);
        Ok(())
    }

    fn local_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let l = m.list.read(cx);
        l.mailbox.as_deref().is_some_and(super::is_local) && l.count > 0 && !l.rows.1.is_empty()
    }

    fn move_across(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let exchange = postman_demo::EXCHANGE.profile;
        let inbox = m.folders.read(cx).mailboxes.iter().find(|f| f.role.as_deref() == Some("inbox") && postman_cache::account_of(&f.id) == exchange);
        let inbox = inbox.map(|f| f.id.clone()).ok_or("no Exchange inbox in the demo")?;
        m.open_mailbox(inbox, cx);
        Ok(())
    }

    fn move_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let l = m.list.read(cx);
        l.selection.len() == 3 && l.bulk_menu.is_some()
    }

    /// Три первых письма — выделены, «В папку ▾» открыт (как щелчки пользователя).
    fn move_wait(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.list.update(cx, |l, cx| {
            if l.rows.1.len() >= 3 && l.selection.is_empty() {
                for i in 0..3 {
                    l.pick(i, true, cx);
                }
                l.bulk_menu = Some(crate::bulk::Drop::Folder);
                cx.notify();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use postman_cache::{Cache, LetterRow, MigrationState, Place, Refused};

    use super::{LOCAL, Mark, across, folder_items, home, key, local_folder, mark_of};

    /// Ключи локальных папок: `~local/<n>`, не путаются с папками учёток; учётка ключа.
    #[test]
    fn local_keys() {
        let k = key(postman_cache::FolderId(7));
        assert_eq!(k, "~local/7");
        assert_eq!(local_folder(&k), Some(postman_cache::FolderId(7)));
        assert_eq!(local_folder("inbox"), None);
        assert_eq!(local_folder("@2/inbox"), None);
        assert_eq!((home(&k), home("inbox"), home("@2/inbox")), (LOCAL, "", "@2"));
    }

    /// Перенос между учётками (S8-15): цель — другая учётка или «На этом компьютере», письмо без связи — всегда
    /// ядром; внутри учётки — прежним переносом.
    #[test]
    fn across_accounts() {
        assert!(!across(&["m1".into(), "m2".into()], "archive"));
        assert!(across(&["m1".into()], "@2/inbox"));
        assert!(across(&["@2/m1".into()], "inbox"));
        assert!(across(&["m1".into()], "~local/3"));
        assert!(across(&["#5".into()], "inbox"), "письмо без связи");
        assert!(!across(&["@2/a".into(), "@2/b".into()], "@2/archive"));
    }

    /// «В папку ▾»: все учётки и «На этом компьютере», без открытой; чужая учётка — подписью перед именем.
    #[test]
    fn folder_menu_lists_all_accounts() {
        let order: Vec<String> = ["inbox", "archive", "@2/inbox", "~local/3"].iter().map(|s| s.to_string()).collect();
        let names: HashMap<String, String> = [("inbox", "Входящие"), ("archive", "Архив"), ("@2/inbox", "Входящие"), ("~local/3", "Архив 2024")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let accounts: HashMap<String, String> = [("", "me@corp"), ("@2", "Exchange")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        let got = folder_items(&order, &names, &accounts, Some("inbox"));
        let labels: Vec<String> = got.iter().map(|t| t.label()).collect();
        assert_eq!(labels, ["Архив", "Exchange · Входящие", "На этом компьютере · Архив 2024"]);
        // Из папки другой учётки — сначала её папки, затем основная.
        let got = folder_items(&order, &names, &accounts, Some("@2/inbox"));
        let labels: Vec<String> = got.iter().map(|t| t.label()).collect();
        assert_eq!(labels, ["me@corp · Входящие", "me@corp · Архив", "На этом компьютере · Архив 2024"]);
    }

    /// Значки строки (макет S8-D п. 3): ждёт переноса (с состоянием задания), только на этом компьютере, больше
    /// предела; синхронизированное и письмо вне учётки — без значка.
    #[test]
    fn row_marks() {
        let row = |place| LetterRow { place, size: 180, ..LetterRow::default() };
        let job = |_| MigrationState::Paused;
        assert_eq!(mark_of(&row(Place::Synced), Some("@2"), job), None);
        assert_eq!(mark_of(&row(Place::Local), None, job), None, "в «На этом компьютере» значков нет");
        assert_eq!(
            mark_of(&row(Place::Outbound(postman_cache::JobId(1))), Some("@2"), job),
            Some(Mark::Pending { account: "@2".into(), state: MigrationState::Paused })
        );
        assert_eq!(mark_of(&row(Place::Local), Some(""), job), Some(Mark::Local { account: String::new() }));
        assert_eq!(mark_of(&row(Place::Refused(Refused::TooLarge)), Some(""), job), Some(Mark::TooLarge { account: String::new(), size: 180 }));
        assert_eq!(mark_of(&row(Place::Refused(Refused::Rejected)), Some(""), job), Some(Mark::Local { account: String::new() }));
    }

    /// Окно списка по `FolderId`: локальная папка демо — письма ссылками `#…`, с ключом своей папки.
    #[test]
    fn local_folder_pages() {
        let cache = Cache::open_in_memory().unwrap();
        let f = cache.create_folder(None, "Архив").unwrap();
        assert_eq!(super::local_rows(&cache).iter().map(|m| (m.id.clone(), m.total)).collect::<Vec<_>>(), [(key(f), 0)]);
        assert_eq!(super::count(&cache, &key(f)), 0);
        assert!(super::page(&cache, &key(f), 0, 10).is_empty());
        assert!(super::page(&cache, "no-such", 0, 10).is_empty());
    }

    /// Демо S8-14: «Архив 2024» — 1 200 писем ссылками `#…` без значков; задание переноса — в «Архиве» основной
    /// учётки письма без связи со значком «ждёт переноса», прочие — ключами кэша.
    #[test]
    fn demo_pages_and_marks() {
        let dir = tempfile::Builder::new().prefix("postman-local-").tempdir().unwrap();
        let path = dir.path().join("demo.redb");
        postman_demo::ensure(&path, 50).unwrap();
        let cache = Cache::open(&path).unwrap();
        let archive = super::local_rows(&cache).into_iter().find(|m| m.name == postman_demo::store::ARCHIVE).unwrap();
        assert_eq!(super::count(&cache, &archive.id), postman_demo::store::ARCHIVE_LETTERS);
        let rows = super::page(&cache, &archive.id, 0, 20);
        assert_eq!(rows.len(), 20);
        assert!(rows.iter().all(|r| r.id.starts_with('#') && r.mailbox_ids == [archive.id.clone()]));
        assert!(super::marks(&cache, &rows).is_empty(), "в «На этом компьютере» значков нет");

        postman_demo::store::migration(&cache, postman_demo::store::JobShot::Running).unwrap();
        let (profile, remote) = postman_demo::store::JOB_TARGET;
        let target = postman_cache::scoped(profile, remote);
        let n = super::count(&cache, &target);
        let rows = super::page(&cache, &target, 0, n);
        assert_eq!(rows.len(), n, "окно — все письма папки, и без связи");
        let marks = super::marks(&cache, &rows);
        let pending = postman_demo::store::JOB_TOTAL - postman_demo::store::JOB_DONE;
        assert_eq!(marks.len(), pending);
        assert!(marks.values().all(|m| matches!(m, Mark::Pending { account, state: MigrationState::Running } if account.is_empty())));
    }
}
