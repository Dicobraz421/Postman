//! Выделение писем в списке (S5-08, DESIGN «Действия и состояния»): флажки на месте аватаров, Ctrl+щелчок —
//! добавить или убрать, Shift+щелчок и Shift+↑/↓ — диапазон от якоря, Ctrl+A — все, Esc — снять.
//!
//! Выделение — по id писем, а не по номерам строк: список перечитывается (синхронизация, сортировка, фильтры), а
//! выделенные письма остаются выделенными, где бы они ни оказались; исчезнувшие из списка уходят и из выделения
//! ([`Selection::retain`], проверка — в фоне по кэшу). Другая папка или поиск — выделение снимается. Без gpui — тесты.
//!
//! Ctrl+A в папке, где загружена только часть писем, — не список id, а состояние [`Whole`]: «выбрано всё», письма
//! определит сервер в момент действия (`postman_core::sweep`). Строки в окне при этом тоже в `ids`: сводка и подсветка
//! работают как обычно, но действие идёт над всей выборкой.

use std::collections::HashSet;

use postman_core::model::{Filter, Selector};

/// Выбрано всё по условию на сервере: список id не нужен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Whole {
    pub(crate) mailbox: String,
    pub(crate) filter: Option<Filter>,
    /// Снятые с выделения письма.
    pub(crate) except: HashSet<String>,
    /// Всего писем по условию на сервере; `None` — ответа ещё нет.
    pub(crate) total: Option<u64>,
}

/// Выделенные письма и якорь диапазона.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    /// По порядку выделения (сводка показывает первые).
    ids: Vec<String>,
    set: HashSet<String>,
    /// Якорь Shift-диапазона: письмо и его номер в списке (номер обновляется после перечитывания).
    anchor: Option<(String, usize)>,
    /// Что выделено до Shift-диапазона: диапазон заменяет только свою часть (Ctrl+щелчок, затем Shift+щелчок).
    base: Vec<String>,
    /// Выбрано всё на сервере.
    whole: Option<Whole>,
    /// Выбрано только загруженное: условие на сервере невыразимо, полоса говорит об этом.
    partial: bool,
}

impl Selection {
    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.whole.is_none()
    }

    /// Выбрано писем: при «выбрано всё» — по ответу сервера, без снятых вручную.
    pub(crate) fn len(&self) -> usize {
        match self.whole.as_ref().and_then(|w| w.total.map(|t| (t, w.except.len()))) {
            Some((total, except)) => usize::try_from(total).unwrap_or(usize::MAX).saturating_sub(except),
            None => self.ids.len(),
        }
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        match &self.whole {
            Some(w) => !w.except.contains(id),
            None => self.set.contains(id),
        }
    }

    pub(crate) fn whole(&self) -> Option<&Whole> {
        self.whole.as_ref()
    }

    pub(crate) fn partial(&self) -> bool {
        self.partial
    }

    /// Выборка для ядра; `None` — выделен список id.
    pub(crate) fn selector(&self) -> Option<Selector> {
        self.whole.as_ref().map(|w| Selector { mailbox: w.mailbox.clone(), filter: w.filter.clone(), except: w.except.iter().cloned().collect() })
    }

    /// К списку id ([`Selection::select_all`]) — «выбрано всё» на сервере.
    pub(crate) fn set_whole(&mut self, mailbox: String, filter: Option<Filter>, total: Option<u64>) {
        self.whole = Some(Whole { mailbox, filter, except: HashSet::new(), total });
    }

    /// Ответ сервера: сколько писем по условию.
    pub(crate) fn set_total(&mut self, total: u64) {
        if let Some(w) = &mut self.whole {
            w.total = Some(total);
        }
    }

    /// Сервер условие не выразил: «выбрано всё» — обратно к загруженному, полоса говорит об этом.
    pub(crate) fn fall_back(&mut self) {
        self.whole = None;
        self.partial = true;
    }

    pub(crate) fn set_partial(&mut self) {
        self.partial = true;
    }

    pub(crate) fn ids(&self) -> &[String] {
        &self.ids
    }

    pub(crate) fn anchor(&self) -> Option<usize> {
        self.anchor.as_ref().map(|(_, i)| *i)
    }

    fn add(&mut self, id: &str) {
        if let Some(w) = &mut self.whole {
            w.except.remove(id);
        }
        if self.set.insert(id.to_owned()) {
            self.ids.push(id.to_owned());
        }
    }

    fn remove(&mut self, id: &str) {
        if let Some(w) = &mut self.whole {
            w.except.insert(id.to_owned());
        }
        if self.set.remove(id) {
            self.ids.retain(|x| x != id);
        }
    }

    /// Снять всё.
    pub(crate) fn clear(&mut self) {
        *self = Selection::default();
    }

    /// Ctrl+щелчок или флажок: письмо `id` (строка `index`) — в выделение или из него; оно — новый якорь.
    pub(crate) fn toggle(&mut self, id: &str, index: usize) {
        if self.contains(id) {
            self.remove(id);
        } else {
            self.add(id);
        }
        self.anchor = Some((id.to_owned(), index));
        self.base = self.ids.clone();
    }

    /// Строка беседы (вид «как Outlook», S5-05б): все её письма — в выделение, если хоть одного там нет, иначе — из
    /// него; якорь — строка.
    pub(crate) fn toggle_all(&mut self, ids: &[String], index: usize) {
        let Some(first) = ids.first() else { return };
        if ids.iter().all(|id| self.contains(id)) {
            ids.iter().for_each(|id| self.remove(id));
        } else {
            ids.iter().for_each(|id| self.add(id));
        }
        self.anchor = Some((first.clone(), index));
        self.base = self.ids.clone();
    }

    /// Начать с одного письма (оно же якорь): Shift+↑/↓ от открытого письма.
    pub(crate) fn start(&mut self, id: &str, index: usize) {
        if self.anchor.is_none() {
            self.clear();
            self.add(id);
            self.anchor = Some((id.to_owned(), index));
            self.base = Vec::new();
        }
    }

    /// Shift: диапазон от якоря до письма `to` — `range` — id строк диапазона по порядку списка (от меньшего номера
    /// к большему). Выделенное до диапазона Ctrl+щелчками остаётся; прежний Shift-диапазон заменяется. При «выбрано
    /// всё» диапазон ничего не добавляет: всё уже выбрано.
    pub(crate) fn set_range(&mut self, range: &[String]) {
        if self.whole.is_some() {
            return;
        }
        let base = std::mem::take(&mut self.base);
        let anchor = self.anchor.take();
        self.ids.clear();
        self.set.clear();
        for id in base.iter().chain(range) {
            self.add(id);
        }
        self.anchor = anchor;
        self.base = base;
    }

    /// Ctrl+A: все письма списка (по порядку списка); якоря нет.
    pub(crate) fn select_all(&mut self, ids: impl IntoIterator<Item = String>) {
        self.clear();
        for id in ids {
            self.add(&id);
        }
        self.base = self.ids.clone();
    }

    /// Список перечитан: остаются только письма, которые в нём есть; якорь — на новом номере своего письма
    /// (`index_of`), письма нет — якоря нет. `true` — выделение изменилось.
    pub(crate) fn retain(&mut self, alive: &HashSet<String>, index_of: impl Fn(&str) -> Option<usize>) -> bool {
        let before = self.ids.len();
        self.ids.retain(|id| alive.contains(id));
        self.set.retain(|id| alive.contains(id));
        self.base.retain(|id| alive.contains(id));
        if let Some((id, i)) = &mut self.anchor {
            match index_of(id) {
                Some(n) => *i = n,
                None if !alive.contains(id.as_str()) => self.anchor = None,
                None => {}
            }
        }
        self.ids.len() != before
    }
}

/// Номера строк Shift-диапазона от якоря `anchor` до `to` включительно, по порядку списка.
pub(crate) fn span(anchor: usize, to: usize) -> std::ops::RangeInclusive<usize> {
    anchor.min(to)..=anchor.max(to)
}

#[cfg(test)]
mod tests {
    use super::{Selection, span};
    use std::collections::HashSet;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Ctrl+щелчки — по одному; Shift+щелчок — диапазон от последнего Ctrl-щелчка, прежние Ctrl-письма остаются.
    #[test]
    fn ranges_from_the_anchor_keep_ctrl_picks() {
        let mut s = Selection::default();
        s.toggle("a", 0);
        s.toggle("e", 4);
        assert_eq!(s.ids(), ids(&["a", "e"]));
        assert_eq!(s.anchor(), Some(4));
        // Shift+щелчок по строке 6: диапазон 4..=6.
        assert_eq!(span(4, 6), 4..=6);
        s.set_range(&ids(&["e", "f", "g"]));
        assert_eq!(s.ids(), ids(&["a", "e", "f", "g"]));
        // Другой Shift-конец (вверх до 2) — прежний диапазон заменён, якорь тот же.
        assert_eq!(span(4, 2), 2..=4);
        s.set_range(&ids(&["c", "d", "e"]));
        assert_eq!(s.ids(), ids(&["a", "e", "c", "d"]));
        assert_eq!(s.anchor(), Some(4));
        // Ctrl+щелчок по выделенному — снять.
        s.toggle("c", 2);
        assert!(!s.contains("c") && s.len() == 3);
    }

    /// «Выбрано всё»: число — с сервера, письмо за окном считается выбранным, снятое вручную — идёт в выборку `except` и
    /// возвращается повторным щелчком; снять всё — состояния нет.
    #[test]
    fn whole_selection_is_a_state_not_a_list() {
        let mut s = Selection::default();
        s.select_all(ids(&["a", "b"]));
        s.set_whole("in".into(), None, Some(1997));
        assert_eq!(s.len(), 1997);
        assert!(s.contains("за окном"));
        s.toggle("a", 0);
        assert_eq!((s.len(), s.contains("a")), (1996, false));
        assert_eq!(s.selector().unwrap().except, ids(&["a"]));
        s.toggle("a", 0);
        assert_eq!((s.len(), s.contains("a")), (1997, true));
        assert!(s.selector().unwrap().except.is_empty());
        s.set_range(&ids(&["b"]));
        assert_eq!(s.len(), 1997, "диапазон не сужает «выбрано всё»");
        s.clear();
        assert!(s.is_empty() && s.selector().is_none());
    }

    /// Ответа сервера ещё нет: число — загруженное. Сервер условие не выразил — обратно к загруженному с пометкой.
    #[test]
    fn whole_selection_waits_for_the_count_and_falls_back() {
        let mut s = Selection::default();
        s.select_all(ids(&["a", "b"]));
        s.set_whole("in".into(), Some(postman_core::model::Filter::HasAttachment(true)), None);
        assert_eq!(s.len(), 2);
        assert!(s.whole().is_some_and(|w| w.total.is_none()));
        s.set_total(300);
        assert_eq!(s.len(), 300);
        s.fall_back();
        assert_eq!((s.len(), s.partial(), s.selector()), (2, true, None));
        assert!(s.contains("a") && !s.contains("c"));
    }

    /// Строка беседы — все её письма: щелчок добавляет недостающие, повторный снимает все.
    #[test]
    fn thread_row_picks_the_whole_conversation() {
        let mut s = Selection::default();
        s.toggle("a1", 3);
        s.toggle_all(&ids(&["a3", "a2", "a1"]), 0);
        assert_eq!(s.ids(), ids(&["a1", "a3", "a2"]));
        assert_eq!(s.anchor(), Some(0));
        s.toggle_all(&ids(&["a3", "a2", "a1"]), 0);
        assert!(s.is_empty());
        s.toggle_all(&[], 5);
        assert_eq!(s.anchor(), Some(0), "пустая беседа ничего не меняет");
    }

    /// Shift+↓ от открытого письма: начало — оно само, дальше — диапазон.
    #[test]
    fn shift_arrows_start_at_the_open_letter() {
        let mut s = Selection::default();
        s.start("b", 1);
        assert_eq!((s.ids(), s.anchor()), (ids(&["b"]).as_slice(), Some(1)));
        s.set_range(&ids(&["b", "c"]));
        s.start("x", 9);
        assert_eq!(s.ids(), ids(&["b", "c"]), "якорь уже есть — начало не сбрасывает");
        s.set_range(&ids(&["b"]));
        assert_eq!(s.ids(), ids(&["b"]), "назад к якорю — одно письмо");
    }

    /// Список перечитан (пришло новое письмо сверху, одно выделенное удалено на сервере): выделение — по id.
    #[test]
    fn selection_survives_list_updates_by_id() {
        let mut s = Selection::default();
        s.select_all(ids(&["a", "b", "c"]));
        assert_eq!(s.anchor(), None);
        s.toggle("b", 1);
        s.toggle("b", 1);
        assert_eq!(s.anchor(), Some(1));
        let alive: HashSet<String> = ids(&["new", "b", "c"]).into_iter().collect();
        let index_of = |id: &str| ["new", "c", "b"].iter().position(|x| *x == id);
        assert!(s.retain(&alive, index_of));
        assert_eq!(s.ids(), ids(&["c", "b"]));
        assert_eq!(s.anchor(), Some(2), "якорь — на новом месте своего письма");
        assert!(!s.retain(&alive, index_of), "ничего не изменилось");
        // Якорное письмо исчезло — якоря нет.
        let alive: HashSet<String> = ids(&["c"]).into_iter().collect();
        s.retain(&alive, |_| None);
        assert_eq!((s.ids(), s.anchor()), (ids(&["c"]).as_slice(), None));
        s.clear();
        assert!(s.is_empty());
    }
}
