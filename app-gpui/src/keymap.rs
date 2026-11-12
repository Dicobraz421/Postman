//! Свои клавиши (S5-09, «Настройки → Клавиатура»): поверх реестра команд (`commands.rs`) — назначения
//! пользователя из `postman_core::prefs::Prefs::keys`. Здесь — итоговые клавиши команды, где они действуют,
//! конфликты (одно сочетание у двух команд там, где обе действуют) и запись сочетания для таблицы.
//! Привязка клавиш с учётом своих — `commands::bind`.

use std::collections::BTreeMap;

use pets_commands::{Command, Scheme, SchemeKeys, parse_keys};
use postman_core::prefs::{KeyScheme, Prefs};

/// Свои клавиши: id команды → сочетания (пустой список — без клавиши).
pub(crate) type Overrides = BTreeMap<String, Vec<String>>;

/// Клавиши по ОС (MAC-02): на macOS — значками Mac (`ctrl-shift-r` → `⇧⌘R`), иначе — как в Windows.
pub(crate) fn human(keys: &str) -> String {
    human_on(keys, MAC)
}

/// Эта сборка — для macOS: таблица команд в записи Windows, клавиши ОС — [`for_platform`].
pub(crate) const MAC: bool = cfg!(target_os = "macos");

/// Щелчок «добавить к выделению»: Ctrl+щелчок, на macOS — ⌘-щелчок (⌃-щелчок там — контекстное меню, MAC-04).
pub(crate) fn pick_click(m: &gpui_kit::Modifiers) -> bool {
    if MAC { m.platform } else { m.control }
}

/// Запись сочетания для `mac` (macOS) или Windows.
pub(crate) fn human_on(keys: &str, mac: bool) -> String {
    if mac {
        return keys
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(|s| mac_stroke(&for_platform(s, true).unwrap_or_else(|| s.to_owned())))
            .collect::<Vec<_>>()
            .join(" ");
    }
    windows_human(keys)
}

/// `ctrl-shift-r` → `Ctrl+Shift+R`, `pagedown` → `PgDn`.
fn windows_human(keys: &str) -> String {
    let stroke = |s: &str| {
        let mut parts: Vec<&str> = s.split('-').collect();
        // `ctrl--` — клавиша «минус».
        if parts.len() > 1 && parts.last() == Some(&"") {
            parts.pop();
            if let Some(last) = parts.last_mut() {
                *last = "-";
            }
        }
        parts
            .iter()
            .map(|k| match *k {
                "ctrl" => "Ctrl".to_string(),
                "shift" => "Shift".to_string(),
                "alt" => "Alt".to_string(),
                "win" | "super" | "cmd" => "Win".to_string(),
                "escape" => "Esc".to_string(),
                "enter" => "Enter".to_string(),
                "tab" => "Tab".to_string(),
                "space" => "Space".to_string(),
                "backspace" => "Backspace".to_string(),
                "delete" => "Delete".to_string(),
                "insert" => "Insert".to_string(),
                "home" => "Home".to_string(),
                "end" => "End".to_string(),
                "pagedown" => "PgDn".to_string(),
                "pageup" => "PgUp".to_string(),
                "up" => "↑".to_string(),
                "down" => "↓".to_string(),
                "left" => "←".to_string(),
                "right" => "→".to_string(),
                k => k.to_uppercase(),
            })
            .collect::<Vec<_>>()
            .join("+")
    };
    keys.split(' ').filter(|s| !s.is_empty()).map(stroke).collect::<Vec<_>>().join(" ")
}

/// Где действуют клавиши команды — по её контексту.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Окно письма.
    Composer,
    /// Вкладка встречи.
    Meeting,
    /// Раздел «Почта» главного окна.
    Mail,
    /// Раздел «Календарь».
    Calendar,
    /// Раздел «Контакты» (S5-11).
    Contacts,
    /// Раздел «Задачи» (S6-05).
    Tasks,
    /// Палитра команд (S5-10).
    Palette,
    /// Адресная книга письма «Кому…» (T3-03).
    Recipients,
    /// Всё главное окно.
    Main,
}

impl Place {
    pub(crate) fn of(context: Option<&str>) -> Place {
        // «… && !Composer» — клавиши почты вне письма (письмо вкладкой и ответ в области чтения, S5-06б): это почта.
        // «… && !TasksView» — клавиша всего окна, кроме «Задач» (Ctrl+N: письмо, в «Задачах» — задача); так же
        // «!ContactsView» (T2-43: Ctrl+N и Ctrl+Shift+Q в «Контактах» — свои).
        let context = context.map(|c| c.replace("!Composer", "").replace("!ContextMenu", "").replace("!TasksView", "").replace("!ContactsView", ""));
        match context.as_deref() {
            Some(c) if c.contains("Palette") => Place::Palette,
            Some(c) if c.contains("Recipients") => Place::Recipients,
            Some(c) if c.contains("Composer") => Place::Composer,
            Some(c) if c.contains("Meeting") => Place::Meeting,
            Some(c) if c.contains("CalendarView") => Place::Calendar,
            Some(c) if c.contains("ContactsView") => Place::Contacts,
            Some(c) if c.contains("TasksView") => Place::Tasks,
            Some(c) if c.contains("MailView") => Place::Mail,
            _ => Place::Main,
        }
    }

    /// Ключ текста «Где работает» (`settings-keys-where-…`).
    pub(crate) fn key(self) -> &'static str {
        match self {
            Place::Composer => "settings-keys-where-composer",
            Place::Meeting => "settings-keys-where-meeting",
            Place::Mail => "settings-keys-where-mail",
            Place::Calendar => "settings-keys-where-calendar",
            Place::Contacts => "settings-keys-where-contacts",
            Place::Tasks => "settings-keys-where-tasks",
            Place::Palette => "settings-keys-where-palette",
            Place::Recipients => "settings-keys-where-recipients",
            Place::Main => "settings-keys-where-main",
        }
    }

    /// Клавиши двух команд могут сработать в одном месте: одно окно, и разделы совпадают или один из них — всё окно.
    pub(crate) fn overlaps(self, other: Place) -> bool {
        use Place::*;
        match (self, other) {
            (Composer, Composer) | (Meeting, Meeting) | (Palette, Palette) | (Recipients, Recipients) => true,
            (Composer | Meeting | Palette | Recipients, _) | (_, Composer | Meeting | Palette | Recipients) => false,
            (Main, _) | (_, Main) => true,
            (a, b) => a == b,
        }
    }
}

/// Клавиши команды можно поменять. Клавиши 1–9 — категории таблицы (S5-02): по одной на категорию, меняются
/// в «Категориях».
pub(crate) fn editable(c: &Command) -> bool {
    c.id != "mail.categorize"
}

/// Каноническая запись сочетания (модификаторы по алфавиту) — для сравнения.
fn canon(keys: &str) -> String {
    parse_keys(keys).unwrap_or_else(|_| keys.to_owned())
}

// --- схемы (S5-17) ---

/// Столбец таблицы «Клавиатура»: клавиши схемы «Outlook» (`Command::keys`, действуют всегда) или слоя «Одиночные
/// клавиши» (`commands::SINGLE`, только в этой схеме).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Column {
    Outlook,
    Single,
}

/// Выбранная схема и свои клавиши обоих столбцов — срез настроек (`Prefs::key_scheme`, `keys`, `keys_single`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KeySet {
    pub(crate) scheme: KeyScheme,
    pub(crate) outlook: Overrides,
    pub(crate) single: Overrides,
}

impl KeySet {
    pub(crate) fn of(p: &Prefs) -> KeySet {
        KeySet { scheme: p.key_scheme, outlook: p.keys.clone(), single: p.keys_single.clone() }
    }

    /// Записать свои клавиши обратно в настройки.
    pub(crate) fn store(self, p: &mut Prefs) {
        p.keys = self.outlook;
        p.keys_single = self.single;
    }

    /// Столбец, который правит «Изменить»: выбранной схемы.
    pub(crate) fn column(&self) -> Column {
        match self.scheme {
            KeyScheme::Outlook => Column::Outlook,
            KeyScheme::Single => Column::Single,
        }
    }

    fn map(&self, col: Column) -> &Overrides {
        match col {
            Column::Outlook => &self.outlook,
            Column::Single => &self.single,
        }
    }

    fn map_mut(&mut self, col: Column) -> &mut Overrides {
        match col {
            Column::Outlook => &mut self.outlook,
            Column::Single => &mut self.single,
        }
    }
}

/// Слой схемы у команды (одна запись на команду).
fn layer_of<'a>(layer: &'a Scheme, c: &Command) -> Option<&'a SchemeKeys> {
    layer.keys.iter().find(|k| k.command == c.id)
}

/// Клавиши столбца по умолчанию: Outlook — `Command::keys`, «Одиночные» — слой схемы.
fn defaults(layer: &Scheme, c: &Command, col: Column) -> Vec<String> {
    let keys: &[&str] = match col {
        Column::Outlook => c.keys,
        Column::Single => layer_of(layer, c).map_or(&[], |k| k.keys),
    };
    keys.iter().map(|k| k.to_string()).collect()
}

/// Итоговые клавиши столбца: свои или по умолчанию.
pub(crate) fn column_keys(layer: &Scheme, set: &KeySet, c: &Command, col: Column) -> Vec<String> {
    match set.map(col).get(c.id).filter(|_| editable(c)) {
        Some(keys) => keys.clone(),
        None => defaults(layer, c, col),
    }
}

/// Где действуют клавиши столбца: Outlook — контекст команды; «Одиночные» — из слоя, а без записи в слое — контекст
/// команды вне полей ввода (одиночные буквы в поле — текст).
pub(crate) fn column_context(layer: &Scheme, c: &Command, col: Column) -> Option<String> {
    match (col, layer_of(layer, c)) {
        (Column::Outlook, _) => c.context.map(str::to_owned),
        (Column::Single, Some(k)) => k.context.map(str::to_owned),
        (Column::Single, None) => Some(format!("({}) && !Input", c.context.unwrap_or("Mailer"))),
    }
}

/// Итоговые клавиши команды в столбце Outlook: свои или по умолчанию.
pub(crate) fn effective(c: &Command, overrides: &Overrides) -> Vec<String> {
    match overrides.get(c.id).filter(|_| editable(c)) {
        Some(keys) => keys.clone(),
        None => c.keys.iter().map(|k| k.to_string()).collect(),
    }
}

/// Одна действующая привязка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bound {
    pub(crate) id: &'static str,
    pub(crate) keys: String,
    pub(crate) context: Option<String>,
    pub(crate) column: Column,
}

/// Контексты, которые не бывают вместе: в одном `!Invite`, в другом `Invite` (простые выражения через `&&`).
fn disjoint(a: Option<&str>, b: Option<&str>) -> bool {
    let terms = |c: Option<&str>| -> Option<Vec<String>> {
        let c = c?;
        if c.contains("||") {
            return None;
        }
        Some(c.split("&&").map(|t| t.trim().trim_matches(['(', ')']).trim().to_owned()).collect())
    };
    let (Some(a), Some(b)) = (terms(a), terms(b)) else { return false };
    let clash = |x: &[String], y: &[String]| x.iter().any(|t| t.strip_prefix('!').is_some_and(|n| y.iter().any(|u| u == n)));
    clash(&a, &b) || clash(&b, &a)
}

/// Привязки двух контекстов могут сработать от одного нажатия: одно место (`Place::overlaps`) и контексты не
/// исключают друг друга.
pub(crate) fn same_place(a: Option<&str>, b: Option<&str>) -> bool {
    Place::of(a).overlaps(Place::of(b)) && !disjoint(a, b)
}

/// Все действующие привязки по выбранной схеме со своими клавишами: Outlook, затем слой «Одиночных»; сочетание
/// Outlook, которое слой дал там же, уходит (как `pets_commands::bindings`).
pub(crate) fn bound(commands: &[Command], layer: &Scheme, set: &KeySet) -> Vec<Bound> {
    let mut single = Vec::new();
    if set.scheme == KeyScheme::Single {
        for c in commands.iter().filter(|c| editable(c)) {
            let context = column_context(layer, c, Column::Single);
            for keys in column_keys(layer, set, c, Column::Single) {
                single.push(Bound { id: c.id, keys, context: context.clone(), column: Column::Single });
            }
        }
    }
    let mut out = Vec::new();
    for c in commands {
        for keys in effective(c, &set.outlook) {
            let k = canon(&keys);
            let taken = single.iter().any(|s| canon(&s.keys) == k && same_place(c.context, s.context.as_deref()));
            if !taken {
                out.push(Bound { id: c.id, keys, context: c.context.map(str::to_owned), column: Column::Outlook });
            }
        }
    }
    out.extend(single);
    out
}

/// Одно сочетание у двух команд, которые действуют в одном месте.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Conflict {
    /// Сочетание в записи gpui.
    pub(crate) keys: String,
    pub(crate) first: &'static str,
    pub(crate) second: &'static str,
}

/// Все конфликты действующих привязок.
pub(crate) fn conflicts(commands: &[Command], layer: &Scheme, set: &KeySet) -> Vec<Conflict> {
    let all = bound(commands, layer, set);
    let mut out = Vec::new();
    for (i, b) in all.iter().enumerate() {
        let k = canon(&b.keys);
        for a in all[..i].iter().filter(|a| a.id != b.id && canon(&a.keys) == k) {
            if same_place(a.context.as_deref(), b.context.as_deref()) {
                out.push(Conflict { keys: b.keys.clone(), first: a.id, second: b.id });
            }
        }
    }
    out
}

/// Команда, у которой это сочетание уже действует там, где его получила бы `id` в столбце выбранной схемы.
pub(crate) fn taken_by(commands: &[Command], layer: &Scheme, set: &KeySet, id: &str, keys: &str) -> Option<&'static str> {
    let c = commands.iter().find(|c| c.id == id)?;
    let context = column_context(layer, c, set.column());
    let k = canon(keys);
    bound(commands, layer, set)
        .into_iter()
        .find(|b| b.id != id && canon(&b.keys) == k && same_place(context.as_deref(), b.context.as_deref()))
        .map(|b| b.id)
}

/// Назначить команде `id` сочетание `keys` (вместо прежних) в столбце выбранной схемы. `replace` — снять его там,
/// где оно действовало у других команд. Своё, совпавшее с умолчанием, не хранится. Ошибка — сочетание не разобрано.
pub(crate) fn assign(commands: &[Command], layer: &Scheme, set: &mut KeySet, id: &str, keys: &str, replace: bool) -> Result<(), String> {
    let canonical = parse_keys(keys)?;
    let Some(c) = commands.iter().find(|c| c.id == id).filter(|c| editable(c)) else {
        return Err(format!("{id}: keys are fixed"));
    };
    if replace {
        while let Some(other) = taken_by(commands, layer, set, id, keys) {
            let Some(o) = commands.iter().find(|c| c.id == other) else { break };
            let Some(col) = bound(commands, layer, set).into_iter().find(|b| b.id == other && canon(&b.keys) == canonical).map(|b| b.column) else {
                break;
            };
            let rest: Vec<String> = column_keys(layer, set, o, col).into_iter().filter(|e| canon(e) != canonical).collect();
            set_keys(layer, set, o, col, rest);
        }
    }
    let col = set.column();
    set_keys(layer, set, c, col, vec![keys.to_owned()]);
    Ok(())
}

/// Сбросить клавиши команды в столбце выбранной схемы к умолчанию.
pub(crate) fn reset(set: &mut KeySet, id: &str) {
    let col = set.column();
    set.map_mut(col).remove(id);
}

/// Свои клавиши команды в выбранной схеме.
pub(crate) fn changed(set: &KeySet, id: &str) -> bool {
    set.map(set.column()).contains_key(id)
}

fn set_keys(layer: &Scheme, set: &mut KeySet, c: &Command, col: Column, keys: Vec<String>) {
    let default = defaults(layer, c, col);
    if keys == default {
        set.map_mut(col).remove(c.id);
    } else {
        set.map_mut(col).insert(c.id.to_owned(), keys);
    }
}

/// Клавиша для подсказки у кнопки и в палитре: в «Одиночных клавишах» — своя буква, если есть, иначе Outlook.
pub(crate) fn hint(layer: &Scheme, set: &KeySet, c: &Command) -> Option<String> {
    let single = if set.scheme == KeyScheme::Single { column_keys(layer, set, c, Column::Single) } else { Vec::new() };
    single.into_iter().chain(effective(c, &set.outlook)).next()
}

/// Ревизия клавиш по умолчанию (`Prefs::keys_rev`): растёт, когда реестр снимает или переносит клавиши.
/// 1 — S5-16, правило одного модификатора (`docs/DESIGN.md` «Хоткеи»). 2 — Ctrl+Y «Перейти к папке» (решение владельца
/// 2026-11-06, п. 7).
pub(crate) const KEYS_REV: u32 = 2;

/// Клавиши по умолчанию, которые S5-16 сняла с команд: (команда, сочетание).
const REMOVED_1: &[(&str, &str)] = &[
    ("mail.next", "j"),
    ("mail.prev", "k"),
    ("mail.toggle_flag", "insert"),
    ("mail.due", "ctrl-shift-g"),
    ("calendar.prev_week", "alt-up"),
    ("calendar.next_week", "alt-down"),
    ("calendar.today", "alt-home"),
    ("compose.send", "alt-s"),
    ("meeting.send", "alt-s"),
    ("view.filters", "ctrl-shift-f"),
    ("invite.accept", "a"),
    ("invite.tentative", "t"),
    ("invite.decline", "n"),
];

/// Клавиши по умолчанию, которые S5-16 дала командам.
const ADDED_1: &[(&str, &str)] = &[
    ("mail.due", "insert"),
    ("calendar.prev_week", "pageup"),
    ("calendar.next_week", "pagedown"),
    ("calendar.today", "home"),
    ("tab.new", "ctrl-t"),
];

/// Клавиши по умолчанию, которые дала ревизия 2.
const ADDED_2: &[(&str, &str)] = &[("folder.go", "ctrl-y")];

/// Свои клавиши ревизии `rev` — к [`KEYS_REV`]. Из прежних умолчаний в своих списках (остаются после «Заменить»)
/// снятые клавиши уходят; своё, совпавшее с новым умолчанием, и назначения несуществующих команд не хранятся;
/// новое умолчание, которое пользователь уже отдал другой команде там же, остаётся у его команды (у новой — без
/// этой клавиши). Свои сочетания, выбранные пользователем, не трогаются — даже вне правила.
pub(crate) fn migrate(commands: &[Command], overrides: &mut Overrides, rev: u32) {
    if rev >= KEYS_REV {
        return;
    }
    overrides.retain(|id, _| commands.iter().any(|c| c.id == id && editable(c)));
    if rev < 1 {
        let ids: Vec<String> = overrides.keys().cloned().collect();
        for id in ids {
            let Some(c) = commands.iter().find(|c| c.id == id) else { continue };
            let Some(keys) = overrides.get(&id) else { continue };
            let kept: Vec<String> = keys.iter().filter(|k| !REMOVED_1.iter().any(|(cid, r)| *cid == id && canon(k) == canon(r))).cloned().collect();
            set_outlook(overrides, c, kept);
        }
    }
    let added = (rev < 1).then_some(ADDED_1).into_iter().chain((rev < 2).then_some(ADDED_2)).flatten();
    for &(id, key) in added {
        let Some(c) = commands.iter().find(|c| c.id == id) else { continue };
        let set = KeySet { outlook: overrides.clone(), ..KeySet::default() };
        if overrides.contains_key(id) || taken_by(commands, &NO_LAYER, &set, id, key).is_none() {
            continue;
        }
        let rest: Vec<String> = c.keys.iter().filter(|k| canon(k) != canon(key)).map(|k| k.to_string()).collect();
        set_outlook(overrides, c, rest);
    }
}

/// Слоя нет (схема «Outlook»): перевод своих клавиш S5-16 — только по клавишам Outlook.
const NO_LAYER: Scheme = Scheme { id: "outlook", keys: &[] };

fn set_outlook(overrides: &mut Overrides, c: &Command, keys: Vec<String>) {
    let default: Vec<String> = c.keys.iter().map(|k| k.to_string()).collect();
    if keys == default {
        overrides.remove(c.id);
    } else {
        overrides.insert(c.id.to_owned(), keys);
    }
}

/// Сочетание нажатия для таблицы: `None` — одни модификаторы (ждём клавишу). На macOS нажатие ⌘ записывается
/// в таблицу как `ctrl` (запись Windows, [`from_platform`]).
pub(crate) fn recorded(unparsed: &str) -> Option<String> {
    let unparsed = unparsed.to_lowercase();
    let unparsed = unparsed.as_str();
    let last = unparsed.rsplit('-').next().unwrap_or_default();
    let minus = unparsed == "-" || unparsed.ends_with("--");
    if !minus && ["shift", "control", "alt", "platform", "function", "ctrl", "win", "cmd", "super", "fn", ""].contains(&last) {
        return None;
    }
    let keys = parse_keys(unparsed).ok()?;
    if MAC { parse_keys(&from_platform(&keys)).ok() } else { Some(keys) }
}

// --- клавиши macOS (MAC-02) ---

/// Модификаторы и клавиша одного нажатия: `ctrl-shift-r` → (["ctrl", "shift"], "r"); `ctrl--` — клавиша «минус».
fn split_stroke(stroke: &str) -> (Vec<&str>, &str) {
    const MODS: [&str; 7] = ["ctrl", "alt", "shift", "cmd", "super", "win", "fn"];
    let mut mods = Vec::new();
    let mut rest = stroke;
    while let Some((head, tail)) = rest.split_once('-') {
        if tail.is_empty() || !MODS.contains(&head) {
            break;
        }
        mods.push(head);
        rest = tail;
    }
    (mods, rest)
}

fn join_stroke(mods: &[&str], key: &str) -> String {
    mods.iter().copied().chain(std::iter::once(key)).collect::<Vec<_>>().join("-")
}

/// Сочетания, которые на macOS держит система или меню приложения (MAC-02): ⌘Q выход, ⌘H скрыть, ⌥⌘H скрыть
/// остальные, ⌘M свернуть, ⌘Tab и ⌘` — переключение, ⌘Space — Spotlight.
const MAC_SYSTEM: &[&str] = &["cmd-q", "cmd-h", "alt-cmd-h", "cmd-m", "cmd-tab", "cmd-shift-tab", "cmd-`", "cmd-space"];

/// Замена клавиши, которую на macOS заняла система: Ctrl+Q («Прочитано») — ⇧⌘U, как в «Почте» Apple.
const MAC_MOVED: &[(&str, &str)] = &[("cmd-q", "cmd-shift-u")];

/// Сочетание таблицы (запись Windows, `ctrl` — главный модификатор) для ОС. macOS: `ctrl` → `cmd` (⌘), кроме
/// Ctrl+Tab и Ctrl+Shift+Tab (⌘Tab — переключатель приложений); `cmd` таблицы (клавиша Windows, команды её не берут)
/// → физический Control (⌃, MAC-04: так записывается ⌃ в «Клавиатуре»); Alt (Option) команды не берут — он печатает
/// символы (`None`: на macOS такой клавиши нет); занятое системой — [`MAC_MOVED`] или `None`. Windows — как есть.
pub(crate) fn for_platform(keys: &str, mac: bool) -> Option<String> {
    if !mac {
        return Some(keys.to_owned());
    }
    let mut out = Vec::new();
    for stroke in keys.split(' ').filter(|s| !s.is_empty()) {
        let (mods, key) = split_stroke(stroke);
        if mods.contains(&"alt") {
            return None;
        }
        let mods: Vec<&str> = mods.into_iter().map(|m| swap_main(m, key)).collect();
        let stroke = parse_keys(&join_stroke(&mods, key)).unwrap_or_else(|_| join_stroke(&mods, key));
        let stroke = match MAC_MOVED.iter().find(|(from, _)| *from == stroke) {
            Some((_, to)) => (*to).to_owned(),
            None if MAC_SYSTEM.contains(&stroke.as_str()) => return None,
            None => stroke,
        };
        out.push(stroke);
    }
    Some(out.join(" "))
}

/// Клавиши этой ОС — для привязки gpui; `None` — на этой ОС у сочетания нет клавиши.
pub(crate) fn for_os(keys: &str) -> Option<String> {
    for_platform(keys, MAC)
}

/// Главный модификатор таблицы ↔ macOS: `ctrl` ↔ `cmd` (⌘ — то, что Ctrl на Windows; физический ⌃ — `cmd` таблицы).
/// С Tab — как есть: ⌃Tab переключает вкладки, ⌘Tab — приложения.
fn swap_main<'a>(m: &'a str, key: &str) -> &'a str {
    match m {
        _ if key == "tab" => m,
        "ctrl" => "cmd",
        "cmd" => "ctrl",
        _ => m,
    }
}

/// Нажатие на macOS — в запись таблицы: `cmd` → `ctrl` (⌘ — главный модификатор), физический ⌃ → `cmd` (MAC-04),
/// ⇧⌘U → Ctrl+Q ([`MAC_MOVED`]).
fn from_platform(keys: &str) -> String {
    keys.split(' ')
        .filter(|s| !s.is_empty())
        .map(|stroke| {
            let stroke = MAC_MOVED.iter().find(|(_, to)| *to == stroke).map_or(stroke, |(from, _)| *from);
            let (mods, key) = split_stroke(stroke);
            let mods: Vec<&str> = mods.into_iter().map(|m| swap_main(m, key)).collect();
            join_stroke(&mods, key)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Нажатие значками Mac: модификаторы в порядке ⌃⌥⇧⌘, затем клавиша (`cmd-shift-r` → `⇧⌘R`).
fn mac_stroke(stroke: &str) -> String {
    let (mods, key) = split_stroke(stroke);
    let mut s = String::new();
    for (m, sign) in [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("cmd", "⌘")] {
        if mods.contains(&m) {
            s.push_str(sign);
        }
    }
    s.push_str(&match key {
        "enter" => "↩".to_owned(),
        "escape" => "⎋".to_owned(),
        "tab" => "⇥".to_owned(),
        "space" => "Space".to_owned(),
        "backspace" => "⌫".to_owned(),
        "delete" => "⌦".to_owned(),
        "home" => "↖".to_owned(),
        "end" => "↘".to_owned(),
        "pageup" => "⇞".to_owned(),
        "pagedown" => "⇟".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        k => k.to_uppercase(),
    });
    s
}

/// Команды для таблицы «Клавиатура» по поиску: название, id или клавиша (`ctrl+r`, `Ctrl+R`).
pub(crate) fn search(query: &str, name: impl Fn(&Command) -> String, layer: &Scheme, set: &KeySet) -> Vec<&'static Command> {
    let q = query.trim().to_lowercase();
    crate::commands::all()
        .iter()
        .filter(|c| {
            if q.is_empty() {
                return true;
            }
            let keys: Vec<String> = [Column::Outlook, Column::Single]
                .into_iter()
                .flat_map(|col| column_keys(layer, set, c, col))
                .map(|k| human(&k).to_lowercase())
                .collect();
            name(c).to_lowercase().contains(&q) || c.id.contains(&q) || keys.iter().any(|k| k.contains(&q))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SINGLE;

    /// Без слоя схемы — таблица теста.
    const NONE: Scheme = Scheme { id: "none", keys: &[] };

    fn outlook(o: &Overrides) -> KeySet {
        KeySet { outlook: o.clone(), ..KeySet::default() }
    }

    const fn cmd(id: &'static str, keys: &'static [&'static str], context: Option<&'static str>) -> Command {
        Command { id, keys, context }
    }

    const TABLE: &[Command] = &[
        cmd("mail.reply", &["ctrl-r"], Some("MailView")),
        cmd("mail.next", &["j"], Some("MailView && !Input")),
        cmd("calendar.today", &["t"], Some("CalendarView")),
        cmd("search.clear", &["escape"], None),
        cmd("compose.close", &["escape"], Some("Composer || (Composer > Input)")),
        cmd("tab.close", &["ctrl-w"], Some("Mailer")),
    ];

    /// Реестр Postman без своих клавиш — без конфликтов (Esc в окне письма и в главном — разные окна).
    #[test]
    fn registry_has_no_conflicts() {
        assert_eq!(conflicts(crate::commands::all(), &SINGLE, &KeySet::default()), vec![]);
        let single = KeySet { scheme: KeyScheme::Single, ..KeySet::default() };
        assert_eq!(conflicts(crate::commands::all(), &SINGLE, &single), vec![], "A у приглашения и «Ответить всем» — не вместе");
        assert_eq!(conflicts(TABLE, &NONE, &KeySet::default()), vec![]);
    }

    /// Своё сочетание, занятое другой командой там же, — конфликт; в другом разделе или окне — нет.
    #[test]
    fn override_conflicts_where_both_work() {
        let mut o = Overrides::new();
        o.insert("mail.next".into(), vec!["ctrl-r".into()]);
        let c = conflicts(TABLE, &NONE, &outlook(&o));
        assert_eq!(c, vec![Conflict { keys: "ctrl-r".into(), first: "mail.reply", second: "mail.next" }]);
        // Календарь и почта — разные разделы.
        let mut o = Overrides::new();
        o.insert("calendar.today".into(), vec!["j".into()]);
        assert_eq!(conflicts(TABLE, &NONE, &outlook(&o)), vec![]);
        // «Всё окно» пересекается с любым разделом главного окна, но не с окном письма.
        let mut o = Overrides::new();
        o.insert("tab.close".into(), vec!["shift-ctrl-r".into()]);
        o.insert("mail.reply".into(), vec!["ctrl-shift-r".into()]);
        assert_eq!(conflicts(TABLE, &NONE, &outlook(&o)).len(), 1, "модификаторы в любом порядке — одно сочетание");
        assert_eq!(taken_by(TABLE, &NONE, &KeySet::default(), "compose.close", "escape"), None);
        assert_eq!(taken_by(TABLE, &NONE, &KeySet::default(), "tab.close", "ctrl-r"), Some("mail.reply"));
    }

    /// «Заменить» снимает сочетание с прежней команды; без замены — оба (конфликт виден); сброс — умолчание.
    #[test]
    fn assign_replace_and_reset() {
        let mut o = KeySet::default();
        assign(TABLE, &NONE, &mut o, "tab.close", "ctrl-r", false).unwrap();
        assert_eq!(conflicts(TABLE, &NONE, &o).len(), 1);
        let mut o = KeySet::default();
        assign(TABLE, &NONE, &mut o, "tab.close", "ctrl-r", true).unwrap();
        assert_eq!(o.outlook.get("mail.reply"), Some(&vec![]), "у «Ответить» клавиши больше нет");
        assert_eq!(o.outlook.get("tab.close"), Some(&vec!["ctrl-r".to_string()]));
        assert_eq!(conflicts(TABLE, &NONE, &o), vec![]);
        reset(&mut o, "tab.close");
        assert_eq!(effective(&TABLE[5], &o.outlook), ["ctrl-w"]);
        // Назначили то же, что по умолчанию, — своего назначения нет.
        let mut o = KeySet::default();
        assign(TABLE, &NONE, &mut o, "mail.reply", "ctrl-r", false).unwrap();
        assert_eq!(o, KeySet::default());
        assert!(assign(TABLE, &NONE, &mut o, "mail.reply", "ctrl-шум", false).is_err());
    }

    /// S5-16: свои клавиши прежней ревизии — без снятых умолчаний; совпало с новым умолчанием — своего нет; новое
    /// умолчание, занятое своим, — у своего; своё вне правила остаётся; ревизия уже новая — ничего.
    #[test]
    fn overrides_migrate_from_before_s5_16() {
        let mut o = Overrides::new();
        // «Заменить» отдал ↓ другой команде: у «Следующее» осталось прежнее J.
        o.insert("mail.next".into(), vec!["j".into()]);
        o.insert("mail.select_down".into(), vec!["down".into()]);
        // «Заменить» отдал Insert «Напомнить»: у флага — пусто (теперь это и есть умолчание).
        o.insert("mail.remind".into(), vec!["insert".into()]);
        o.insert("mail.toggle_flag".into(), vec![]);
        // Свой Alt+S к Ctrl+Enter у письма — только прежнее умолчание Alt+S уходит.
        o.insert("compose.send".into(), vec!["ctrl-enter".into(), "alt-s".into(), "ctrl-s".into()]);
        // Своё вне правила — выбор пользователя.
        o.insert("mail.reply".into(), vec!["alt-r".into()]);
        // Ctrl+T у пересылки — новая «Новая вкладка» без клавиши.
        o.insert("mail.forward".into(), vec!["ctrl-t".into()]);
        o.insert("no.such_command".into(), vec!["f9".into()]);
        migrate(crate::commands::all(), &mut o, 0);
        assert_eq!(o.get("mail.next"), Some(&vec![]), "J ушла, ↓ — у другой команды");
        assert_eq!(o.get("mail.toggle_flag"), None, "пусто — уже умолчание");
        assert_eq!(o.get("mail.remind"), Some(&vec!["insert".to_string()]));
        assert_eq!(o.get("mail.due"), Some(&vec![]), "Insert — у «Напомнить», как выбрал пользователь");
        assert_eq!(o.get("compose.send"), Some(&vec!["ctrl-enter".to_string(), "ctrl-s".to_string()]));
        assert_eq!(o.get("mail.reply"), Some(&vec!["alt-r".to_string()]));
        assert_eq!(o.get("tab.new"), Some(&vec![]));
        assert_eq!(o.get("no.such_command"), None);
        assert_eq!(conflicts(crate::commands::all(), &SINGLE, &outlook(&o)), vec![], "перевод не добавил конфликтов");
        // Только Alt+S было своим — после перевода своего нет.
        let mut o = Overrides::new();
        o.insert("meeting.send".into(), vec!["ctrl-enter".into(), "alt-s".into()]);
        migrate(crate::commands::all(), &mut o, 0);
        assert!(o.is_empty());
        // Ревизия текущая: своё J — выбор пользователя после S5-16.
        let mut o = Overrides::new();
        o.insert("mail.next".into(), vec!["j".into()]);
        migrate(crate::commands::all(), &mut o, KEYS_REV);
        assert_eq!(o.get("mail.next"), Some(&vec!["j".to_string()]));
    }

    /// Ревизия 2: Ctrl+Y у «Перейти к папке» появляется и при своих клавишах; отдал её другой команде — остаётся у него.
    #[test]
    fn ctrl_y_reaches_saved_schemes() {
        let all = crate::commands::all();
        let go = pets_commands::find(all, "folder.go").unwrap();
        let mut o = Overrides::new();
        o.insert("mail.reply".into(), vec!["ctrl-j".into()]);
        migrate(all, &mut o, 1);
        assert_eq!(effective(go, &o), ["ctrl-y"]);
        assert_eq!(o.get("mail.reply"), Some(&vec!["ctrl-j".to_string()]), "свои клавиши на месте");
        // Прежние свои S5-16 ревизия 1 не трогает: снятая клавиша, назначенная после S5-16, — выбор пользователя.
        let mut o = Overrides::new();
        o.insert("mail.next".into(), vec!["j".into()]);
        migrate(all, &mut o, 1);
        assert_eq!(o.get("mail.next"), Some(&vec!["j".to_string()]));
        // Ctrl+Y уже занята своей клавишей: у новой команды её нет, конфликта нет.
        let mut o = Overrides::new();
        o.insert("mail.reply".into(), vec!["ctrl-y".into()]);
        migrate(all, &mut o, 1);
        assert_eq!(o.get("mail.reply"), Some(&vec!["ctrl-y".to_string()]));
        assert_eq!(o.get("folder.go"), Some(&vec![]));
        assert_eq!(conflicts(all, &SINGLE, &outlook(&o)), vec![]);
    }

    #[test]
    fn categories_keys_are_fixed() {
        let c = pets_commands::find(crate::commands::all(), "mail.categorize").unwrap();
        let mut o = Overrides::new();
        o.insert(c.id.into(), vec![]);
        assert_eq!(effective(c, &o).len(), 9);
        let mut set = outlook(&o);
        assert!(assign(crate::commands::all(), &SINGLE, &mut set, c.id, "ctrl-1", false).is_err());
        set.scheme = KeyScheme::Single;
        assert!(assign(crate::commands::all(), &SINGLE, &mut set, c.id, "x", false).is_err());
    }

    /// S5-17: «Одиночные клавиши» — слой поверх Outlook: буквы вне полей ввода, клавиши Outlook на месте, конфликтов
    /// нет; итог — как у `pets_commands::bindings`.
    #[test]
    fn single_scheme_layers_over_outlook() {
        let outlook_only = bound(crate::commands::all(), &SINGLE, &KeySet::default());
        assert!(outlook_only.iter().all(|b| b.column == Column::Outlook));
        assert!(!outlook_only.iter().any(|b| b.keys == "j"));
        let set = KeySet { scheme: KeyScheme::Single, ..KeySet::default() };
        let all = bound(crate::commands::all(), &SINGLE, &set);
        let outlook_keys: usize = crate::commands::all().iter().map(|c| c.keys.len()).sum();
        assert_eq!(all.iter().filter(|b| b.column == Column::Outlook).count(), outlook_keys, "клавиши Outlook не потеряны");
        for b in all.iter().filter(|b| b.column == Column::Single) {
            assert!(b.context.as_deref().is_some_and(|c| c.contains("!Input")), "{} {}: в поле ввода", b.id, b.keys);
        }
        let keys = |id: &str| all.iter().filter(|b| b.id == id).map(|b| b.keys.as_str()).collect::<Vec<_>>();
        assert_eq!(keys("mail.next"), ["down", "j"]);
        assert_eq!(keys("invite.accept"), ["a"]);
        let pets: Vec<(&str, String, Option<String>)> = pets_commands::bindings(crate::commands::all(), Some(&SINGLE), same_place)
            .into_iter()
            .map(|b| (b.command, b.keys.to_owned(), b.context.map(str::to_owned)))
            .collect();
        let mut ours: Vec<(&str, String, Option<String>)> = all.into_iter().map(|b| (b.id, b.keys, b.context)).collect();
        let mut pets = pets;
        ours.sort();
        pets.sort();
        assert_eq!(ours, pets, "те же привязки, порядок слоя — свой");
        // A — «Принять» над приглашением и «Ответить всем» без него: не одно место.
        let ctx = |id: &str| SINGLE.keys.iter().find(|k| k.command == id).and_then(|k| k.context);
        assert!(!same_place(ctx("invite.accept"), ctx("mail.reply_all")));
        assert!(same_place(ctx("mail.reply"), ctx("mail.reply_all")));
    }

    /// Смена схемы со своими клавишами: у каждого столбца свои; «Изменить» и «Сбросить» — в столбце выбранной
    /// схемы; своё Outlook действует и в «Одиночных», своё «Одиночных» — только в них.
    #[test]
    fn scheme_switch_keeps_own_keys_per_column() {
        let mut set = KeySet::default();
        assign(crate::commands::all(), &SINGLE, &mut set, "mail.reply", "ctrl-shift-y", false).unwrap();
        assert_eq!(set.outlook.get("mail.reply"), Some(&vec!["ctrl-shift-y".to_string()]));
        set.scheme = KeyScheme::Single;
        assert!(!changed(&set, "mail.reply"), "в «Одиночных» у «Ответить» своего нет");
        assign(crate::commands::all(), &SINGLE, &mut set, "mail.archive", "y", false).unwrap();
        assert_eq!(set.single.get("mail.archive"), Some(&vec!["y".to_string()]));
        assert!(changed(&set, "mail.archive"));
        let has = |set: &KeySet, id: &str, keys: &str| bound(crate::commands::all(), &SINGLE, set).iter().any(|b| b.id == id && b.keys == keys);
        assert!(has(&set, "mail.reply", "ctrl-shift-y") && has(&set, "mail.reply", "r"));
        assert!(has(&set, "mail.archive", "y") && !has(&set, "mail.archive", "e") && has(&set, "mail.archive", "backspace"));
        // Буква для команды без слоя — вне полей ввода её места.
        assign(crate::commands::all(), &SINGLE, &mut set, "calendar.today", "t", false).unwrap();
        let today = bound(crate::commands::all(), &SINGLE, &set).into_iter().find(|b| b.id == "calendar.today" && b.keys == "t");
        assert_eq!(today.and_then(|b| b.context).as_deref(), Some("(CalendarView && !Input) && !Input"));
        // Занятая буква: «Заменить» снимает её в столбце, где она была.
        assert_eq!(taken_by(crate::commands::all(), &SINGLE, &set, "mail.delete", "r"), Some("mail.reply"));
        assign(crate::commands::all(), &SINGLE, &mut set, "mail.delete", "r", true).unwrap();
        assert_eq!(set.single.get("mail.reply"), Some(&vec![]));
        assert_eq!(conflicts(crate::commands::all(), &SINGLE, &set), vec![]);
        reset(&mut set, "mail.archive");
        assert!(!set.single.contains_key("mail.archive"));
        // Назад в Outlook: буквы не действуют, своё Outlook на месте.
        set.scheme = KeyScheme::Outlook;
        assert!(!has(&set, "mail.delete", "r") && has(&set, "mail.reply", "ctrl-shift-y"));
        let forward = pets_commands::find(crate::commands::all(), "mail.forward").unwrap();
        assert_eq!(hint(&SINGLE, &set, forward).as_deref(), Some("ctrl-f"));
        set.scheme = KeyScheme::Single;
        assert_eq!(hint(&SINGLE, &set, forward).as_deref(), Some("f"));
    }

    #[test]
    fn keystrokes_are_recorded_without_bare_modifiers() {
        assert_eq!(recorded("ctrl-shift-r").as_deref(), Some("ctrl-shift-r"));
        assert_eq!(recorded("shift-ctrl-k").as_deref(), Some("ctrl-shift-k"));
        assert_eq!(recorded("ctrl-control"), None);
        assert_eq!(recorded("shift"), None);
        assert_eq!(recorded("f5").as_deref(), Some("f5"));
    }

    #[test]
    fn keys_read_like_windows() {
        let human = |k| human_on(k, false);
        assert_eq!(human("ctrl-shift-r"), "Ctrl+Shift+R");
        assert_eq!(human("alt-down"), "Alt+↓");
        assert_eq!(human("j"), "J");
        assert_eq!(human("ctrl--"), "Ctrl+-");
        assert_eq!(human("ctrl-,"), "Ctrl+,");
    }

    /// MAC-02: на macOS Ctrl — это ⌘, Ctrl+Tab остаётся Control, Alt-сочетаний нет, ⌘Q — выход (Ctrl+Q → ⇧⌘U).
    #[test]
    fn keys_follow_macos() {
        let mac = |k| for_platform(k, true);
        assert_eq!(mac("ctrl-n").as_deref(), Some("cmd-n"));
        assert_eq!(mac("ctrl-shift-r").as_deref(), Some("cmd-shift-r"));
        assert_eq!(mac("ctrl-tab").as_deref(), Some("ctrl-tab"));
        assert_eq!(mac("ctrl-shift-tab").as_deref(), Some("ctrl-shift-tab"));
        assert_eq!(mac("ctrl--").as_deref(), Some("cmd--"));
        assert_eq!(mac("ctrl-q").as_deref(), Some("cmd-shift-u"));
        assert_eq!(mac("ctrl-h"), None);
        assert_eq!(mac("alt-s"), None);
        assert_eq!(mac("pagedown").as_deref(), Some("pagedown"));
        assert_eq!(mac("j").as_deref(), Some("j"));
        assert_eq!(for_platform("ctrl-q", false).as_deref(), Some("ctrl-q"));
        assert_eq!(from_platform("cmd-r"), "ctrl-r");
        assert_eq!(from_platform("cmd-shift-u"), "ctrl-q");
        assert_eq!(human_on("ctrl-shift-r", true), "⇧⌘R");
        assert_eq!(human_on("ctrl-,", true), "⌘,");
        assert_eq!(human_on("ctrl-enter", true), "⌘↩");
        assert_eq!(human_on("ctrl-tab", true), "⌃⇥");
        assert_eq!(human_on("ctrl-q", true), "⇧⌘U");
    }

    /// MAC-04: физический Control в «Клавиатуре» записывается не как ⌘, а своим модификатором таблицы (`cmd` — Win,
    /// команды его не берут) и привязывается обратно к ⌃; ⌃Tab — как есть.
    #[test]
    fn physical_control_is_kept_on_macos() {
        assert_eq!(from_platform("ctrl-r"), "cmd-r");
        assert_eq!(from_platform("ctrl-cmd-r"), "cmd-ctrl-r");
        assert_eq!(from_platform("ctrl-tab"), "ctrl-tab");
        assert_eq!(for_platform("cmd-r", true).as_deref(), Some("ctrl-r"));
        assert_eq!(for_platform(&from_platform("ctrl-shift-k"), true).as_deref(), Some("ctrl-shift-k"));
        assert_eq!(for_platform(&from_platform("cmd-shift-k"), true).as_deref(), Some("cmd-shift-k"));
        assert_eq!(human_on("cmd-r", true), "⌃R");
    }

    /// Клавиши по умолчанию на macOS не попадают на системные сочетания и не сталкиваются между собой там, где
    /// сталкивались бы и на Windows (перевод взаимно однозначен).
    #[test]
    fn default_keys_fit_macos() {
        let set = KeySet::default();
        let all = bound(crate::commands::all(), &SINGLE, &set);
        for b in &all {
            if let Some(k) = for_platform(&b.keys, true) {
                assert!(!MAC_SYSTEM.contains(&k.as_str()), "{}: {k}", b.id);
                for o in all.iter().filter(|o| o.id != b.id && canon(&o.keys) != canon(&b.keys)) {
                    let same = for_platform(&o.keys, true).is_some_and(|ok| canon(&ok) == canon(&k));
                    assert!(!(same && same_place(b.context.as_deref(), o.context.as_deref())), "{} и {}: {k}", b.id, o.id);
                }
            }
        }
    }
}
