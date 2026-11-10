//! Вкладки окна (S5-01, DESIGN «Главное окно»): заголовок окна — только вкладки. «Почта» — первая, не закрывается;
//! остальные открываются и закрываются (Ctrl+W), переключаются Ctrl+Tab / Ctrl+Shift+Tab. Модель общая:
//! настройки ящика (автоответ, правила), письма (S5-06б: по вкладке на черновик), дальше — настройки (S5-09).
//!
//! Вкладки почты (S5-16, Ctrl+T): ещё один список с областью чтения на своей папке, фильтрах и письме. Части экрана
//! почты одни на всё окно: в них — состояние вкладки-«владельца» ([`Tabs::owner`]); перешли в другую вкладку почты —
//! состояние прежней запоминается ([`MailState`]), новой — ставится в части ([`Tabs::hand_over`]).

use std::collections::HashMap;

use crate::filters::Query;

/// Что во вкладке.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabKind {
    /// Почта, календарь, контакты, задачи (разделы в заголовке) — главная вкладка.
    Main,
    /// Ещё вкладка почты (Ctrl+T, S5-16): список и область чтения со своим состоянием; их — сколько угодно.
    Mail,
    /// Вкладка «Настройки» (`settings_tab.rs`, S5-09).
    Settings,
    /// Письмо (`composer::Composer` с этим `id`); черновиков — сколько угодно.
    Compose(u64),
    /// Новая встреча (S5-11, `meeting.rs`): их может быть несколько.
    Meeting,
}

impl TabKind {
    /// Главную вкладку не закрыть.
    pub(crate) fn closable(self) -> bool {
        self != TabKind::Main
    }

    /// Вкладка этого вида одна: повторное открытие показывает её.
    fn single(self) -> bool {
        matches!(self, TabKind::Main | TabKind::Settings)
    }

    /// Во вкладке — список писем и область чтения (главная — в разделе «Почта»).
    pub(crate) fn shows_mail(self) -> bool {
        matches!(self, TabKind::Main | TabKind::Mail)
    }
}

/// Состояние вкладки почты (S5-16): папка, фильтры и режим панели слева, выбранное письмо.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MailState {
    pub(crate) mailbox: Option<String>,
    pub(crate) query: Query,
    /// Слева — панель фильтров, а не папки.
    pub(crate) panel: bool,
    /// Выбранное (и открытое) письмо.
    pub(crate) letter: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    /// Постоянный номер вкладки (id элемента в заголовке, адрес закрытия).
    pub(crate) id: u64,
    pub(crate) kind: TabKind,
}

/// Вкладки по порядку и активная.
#[derive(Debug)]
pub(crate) struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    /// Чьё состояние почты сейчас в частях экрана; `None` — владельца закрыли.
    owner: Option<u64>,
    /// Состояние остальных вкладок почты.
    saved: HashMap<u64, MailState>,
}

impl Default for Tabs {
    fn default() -> Self {
        Tabs { tabs: vec![Tab { id: 0, kind: TabKind::Main }], active: 0, next_id: 1, owner: Some(0), saved: HashMap::new() }
    }
}

impl Tabs {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub(crate) fn active(&self) -> Tab {
        self.tabs.get(self.active).copied().unwrap_or(Tab { id: 0, kind: TabKind::Main })
    }

    /// Вкладка почты, чьё состояние — в частях экрана.
    pub(crate) fn owner(&self) -> Option<u64> {
        self.owner
    }

    /// Запомненное состояние вкладки почты (не владельца).
    pub(crate) fn saved(&self, id: u64) -> Option<&MailState> {
        self.saved.get(&id)
    }

    /// Ctrl+T: новая вкладка почты рядом с остальными, активная, с тем же состоянием, что сейчас в частях экрана
    /// (`current`): прежний владелец запоминает его, новая вкладка становится владельцем — частям менять нечего.
    pub(crate) fn open_mail(&mut self, current: MailState) -> u64 {
        if let Some(owner) = self.owner {
            self.saved.insert(owner, current);
        }
        let id = self.open(TabKind::Mail);
        self.owner = Some(id);
        id
    }

    /// Активна вкладка почты, а в частях экрана — состояние другой: прежний владелец запоминает `current`, активная
    /// становится владельцем; её состояние — поставить в части. `None` — ставить нечего (та же вкладка, не почта или
    /// состояние не запомнено).
    pub(crate) fn hand_over(&mut self, current: impl FnOnce() -> MailState) -> Option<MailState> {
        let active = self.active();
        if !active.kind.shows_mail() || self.owner == Some(active.id) {
            return None;
        }
        if let Some(owner) = self.owner.filter(|o| self.tabs.iter().any(|t| t.id == *o)) {
            self.saved.insert(owner, current());
        }
        self.owner = Some(active.id);
        self.saved.remove(&active.id)
    }

    pub(crate) fn is_active(&self, kind: TabKind) -> bool {
        self.active().kind == kind
    }

    /// Открыть вкладку (одиночную — показать уже открытую) и сделать её активной; её id.
    pub(crate) fn open(&mut self, kind: TabKind) -> u64 {
        if kind.single()
            && let Some(i) = self.tabs.iter().position(|t| t.kind == kind)
        {
            self.active = i;
            return self.tabs[i].id;
        }
        let id = self.open_behind(kind);
        self.active = self.tabs.len() - 1;
        id
    }

    /// Вкладка в конец, активная — та же (T2-11: вкладки прошлого запуска); одиночная — уже открытая.
    pub(crate) fn open_behind(&mut self, kind: TabKind) -> u64 {
        if kind.single()
            && let Some(t) = self.tabs.iter().find(|t| t.kind == kind)
        {
            return t.id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab { id, kind });
        id
    }

    /// Вкладка почты прошлого запуска (T2-11): позади активной, со своим состоянием — его поставит переход в неё.
    pub(crate) fn restore_mail(&mut self, state: MailState) -> u64 {
        let id = self.open_behind(TabKind::Mail);
        self.saved.insert(id, state);
        id
    }

    /// Сделать активной; нет такой — `false`.
    pub(crate) fn activate(&mut self, id: u64) -> bool {
        match self.tabs.iter().position(|t| t.id == id) {
            Some(i) => {
                self.active = i;
                true
            }
            None => false,
        }
    }

    /// Активной — вкладку вида (первую); нет — ничего.
    pub(crate) fn activate_kind(&mut self, kind: TabKind) -> bool {
        match self.tabs.iter().find(|t| t.kind == kind).map(|t| t.id) {
            Some(id) => self.activate(id),
            None => false,
        }
    }

    /// Закрыть вкладку; главную не закрыть. Закрыта активная — активной становится соседняя слева.
    pub(crate) fn close(&mut self, id: u64) -> Option<Tab> {
        let i = self.tabs.iter().position(|t| t.id == id)?;
        if !self.tabs[i].kind.closable() {
            return None;
        }
        let tab = self.tabs.remove(i);
        self.saved.remove(&id);
        if self.owner == Some(id) {
            self.owner = None;
        }
        if self.active > i || (self.active == i && i > 0) {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        Some(tab)
    }

    /// Закрыть вкладку вида, если она открыта.
    pub(crate) fn close_kind(&mut self, kind: TabKind) -> Option<Tab> {
        let id = self.tabs.iter().find(|t| t.kind == kind)?.id;
        self.close(id)
    }

    /// Ctrl+Tab (`1`) / Ctrl+Shift+Tab (`-1`): по кругу.
    pub(crate) fn step(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        if n > 0 {
            self.active = (self.active as isize + delta).rem_euclid(n) as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MailState, TabKind, Tabs};

    #[test]
    fn mail_tab_stays_and_others_open_close_and_cycle() {
        let mut t = Tabs::default();
        assert_eq!(t.active().kind, TabKind::Main);
        assert!(t.close(0).is_none(), "«Почта» не закрывается");
        let s = t.open(TabKind::Settings);
        assert!(t.is_active(TabKind::Settings));
        assert_eq!(t.open(TabKind::Settings), s, "вторая вкладка настроек — та же");
        assert_eq!(t.iter().count(), 2);
        t.step(1);
        assert!(t.is_active(TabKind::Main), "по кругу");
        t.step(-1);
        assert!(t.is_active(TabKind::Settings));
        assert!(t.activate_kind(TabKind::Main));
        assert_eq!(t.close_kind(TabKind::Settings).map(|t| t.id), Some(s));
        assert!(t.is_active(TabKind::Main));
        // Закрыли активную — активна соседняя слева.
        t.open(TabKind::Settings);
        assert_eq!(t.close(s + 1).map(|t| t.kind), Some(TabKind::Settings));
        assert!(t.is_active(TabKind::Main));
        assert!(!t.activate(99));
    }

    /// S5-06б: у каждого черновика своя вкладка; закрыть одну — остальные на месте.
    #[test]
    fn every_draft_has_its_own_tab() {
        let mut t = Tabs::default();
        let a = t.open(TabKind::Compose(7));
        let b = t.open(TabKind::Compose(8));
        assert_ne!(a, b);
        assert!(t.is_active(TabKind::Compose(8)));
        assert_eq!(t.iter().count(), 3);
        assert_eq!(t.close(b).map(|t| t.kind), Some(TabKind::Compose(8)));
        assert!(t.is_active(TabKind::Compose(7)), "активна соседняя слева");
        assert!(t.activate_kind(TabKind::Main));
    }

    /// T2-11: вкладки прошлого запуска открываются позади активной; переход во вкладку почты отдаёт её состояние.
    #[test]
    fn restored_tabs_open_behind() {
        let mut t = Tabs::default();
        let mail = t.restore_mail(at("sent", None));
        let draft = t.open_behind(TabKind::Compose(3));
        assert!(t.is_active(TabKind::Main), "активная — та же");
        assert_eq!(t.iter().map(|t| t.id).collect::<Vec<_>>(), vec![0, mail, draft]);
        assert_eq!(t.open_behind(TabKind::Main), 0, "одиночная — уже открытая");
        assert!(t.activate(mail));
        assert_eq!(t.hand_over(|| at("inbox", None)), Some(at("sent", None)));
    }

    fn at(mailbox: &str, letter: Option<&str>) -> MailState {
        MailState { mailbox: Some(mailbox.into()), letter: letter.map(Into::into), ..Default::default() }
    }

    /// S5-16: вкладки почты (Ctrl+T) — по порядку справа, закрываются, у каждой своё состояние; главная — тоже.
    #[test]
    fn mail_tabs_keep_their_own_state() {
        let mut t = Tabs::default();
        assert_eq!(t.owner(), Some(0), "в частях экрана — главная");
        // Ctrl+T на «Входящих» с письмом: новая вкладка — копия, главная её запомнила.
        let a = t.open_mail(at("inbox", Some("m1")));
        assert!(t.is_active(TabKind::Mail));
        assert_eq!(t.owner(), Some(a));
        assert_eq!(t.saved(0), Some(&at("inbox", Some("m1"))));
        assert_eq!(t.hand_over(|| unreachable!()), None, "владелец — уже активная");
        // Во вкладке A открыли «Отправленные»; ещё Ctrl+T, там — «Архив».
        let b = t.open_mail(at("sent", None));
        assert_eq!(t.iter().map(|t| t.id).collect::<Vec<_>>(), [0, a, b], "новые — справа");
        assert_eq!(t.saved(a), Some(&at("sent", None)));
        // Назад в A: B запоминает «Архив», A получает своё.
        assert!(t.activate(a));
        assert_eq!(t.hand_over(|| at("archive", Some("m9"))), Some(at("sent", None)));
        assert_eq!(t.owner(), Some(a));
        assert_eq!(t.saved(b), Some(&at("archive", Some("m9"))));
        assert_eq!(t.saved(a), None, "у владельца состояние — в частях экрана");
        // Не почта (настройки) — ставить нечего, владелец тот же.
        t.open(TabKind::Settings);
        assert_eq!(t.hand_over(|| unreachable!()), None);
        assert_eq!(t.owner(), Some(a));
        // Главная — своё, с самого начала.
        assert!(t.activate(0));
        assert_eq!(t.hand_over(|| at("sent", Some("m2"))), Some(at("inbox", Some("m1"))));
        // Закрыли вкладку-владельца: состояние ушло с ней, следующая вкладка почты получает своё без записи.
        assert!(t.activate(a));
        assert_eq!(t.hand_over(|| at("inbox", Some("m1"))), Some(at("sent", Some("m2"))));
        assert_eq!(t.close(a).map(|t| t.kind), Some(TabKind::Mail));
        assert_eq!(t.owner(), None);
        assert_eq!(t.saved(a), None);
        assert!(t.activate(b));
        assert_eq!(t.hand_over(|| unreachable!()), Some(at("archive", Some("m9"))));
        assert!(t.activate(0));
        assert_eq!(t.hand_over(|| at("drafts", None)), Some(at("inbox", Some("m1"))));
        assert!(t.close(0).is_none(), "главную не закрыть");
        assert!(TabKind::Mail.closable() && TabKind::Mail.shows_mail() && TabKind::Main.shows_mail());
        assert!(!TabKind::Settings.shows_mail());
    }
}
