//! Команды Postman (S2-02): реестр `pets-commands` → действия gpui и привязки клавиш.
//! Действие команды `mail.reply_all` — `mail::ReplyAll`; обработчик вешает экран (`on_action`).
//! У каждой команды есть обработчик на экране (`docs/PARITY.md`); новая — сначала в таблицу, затем обработчик.

use std::rc::Rc;

use gpui_kit::*;
use pets_commands::{Command, Scheme, SchemeKeys};

/// Раздел «Почта» вне полей ввода: клавиши без модификаторов в поиске — текст и курсор. Контекст раздела
/// (`MailView`, `CalendarView`, `ContactsView`, `SettingsView`) ставит корень экрана: в календаре, контактах и
/// настройках клавиши писем молчат. В открытом контекстном меню (S5-08, `ContextMenu`) — тоже: стрелки, Enter и
/// цифры там — клавиши меню.
pub(crate) const LIST: Option<&str> = Some("MailView && !Input && !Composer && !ContextMenu");
/// Одиночная буква «Ответить всем» (A) — не над приглашением без ответа: там A — «Принять» (S5-17).
pub(crate) const LIST_NO_INVITE: Option<&str> = Some("MailView && !Input && !Composer && !ContextMenu && !Invite");
/// Приглашение без ответа в области чтения (корень экрана ставит `Invite`, `mailer.rs`): A / T / N схемы
/// «Одиночные клавиши».
pub(crate) const INVITE: Option<&str> = Some("MailView && Invite && !Input && !Composer && !ContextMenu");
/// Раздел «Почта», включая поле поиска, но не письмо: в ответе в области чтения (S5-06б) Ctrl+R, Ctrl+F, Ctrl+U —
/// клавиши письма, а не списка.
pub(crate) const MAIL: Option<&str> = Some("MailView && !Composer && !ContextMenu");
/// Раздел «Календарь» вне полей ввода: PgUp/PgDn и Home в поле поиска (S5-21) — правка текста.
pub(crate) const CALENDAR: Option<&str> = Some("CalendarView && !Input");
/// Раздел «Контакты» вне полей ввода (Delete, F2 в поиске — правка текста).
pub(crate) const CONTACTS: Option<&str> = Some("ContactsView && !Input");
/// Раздел «Контакты», включая поле поиска.
pub(crate) const CONTACTS_ANY: Option<&str> = Some("ContactsView");
/// Раздел «Задачи» вне полей ввода (Space, Delete, F2, Insert, Enter, стрелки в полях — правка текста).
pub(crate) const TASKS: Option<&str> = Some("TasksView && !Input");
/// Раздел «Задачи», включая поля ввода.
pub(crate) const TASKS_ANY: Option<&str> = Some("TasksView");
/// Весь экран почты (любой раздел), включая поле поиска.
pub(crate) const MAILER: Option<&str> = Some("Mailer");
/// Письмо, в том числе поля ввода: `Composer > Input` глубже привязок поля (`ctrl-enter` в тексте —
/// отправка, а не перевод строки), а при равной глубине побеждает привязка, заданная позже (`bind` — после
/// `gpui_kit::init`).
pub(crate) const COMPOSER: Option<&str> = Some("Composer || (Composer > Input)");
/// Вкладка встречи, как окно письма.
pub(crate) const MEETING: Option<&str> = Some("Meeting || (Meeting > Input)");
/// Вкладка встречи вне полей ввода (S5-23): ←/→ и Enter в поле — курсор и ввод, а не окна помощника.
pub(crate) const MEETING_GRID: Option<&str> = Some("Meeting && !Input");
/// Палитра команд, в том числе её поле ввода: ↑/↓, Enter, Esc поля — клавиши палитры (как у письма, `bind` — после
/// `gpui_kit::init`).
pub(crate) const PALETTE: Option<&str> = Some("Palette || (Palette > Input)");

pub(crate) const fn cmd(id: &'static str, keys: &'static [&'static str], context: Option<&'static str>) -> Command {
    Command { id, keys, context }
}

/// Все команды Postman — из описей частей (`parts.rs`, ADR 0005), по порядку `PARTS`. Хоткеи — схема «Outlook» по правилу одного модификатора (`docs/DESIGN.md` «Хоткеи», S5-16;
/// тест `hotkeys_follow_the_one_modifier_rule`): Ctrl + буква или цифра — команды, без модификатора — навигация и
/// действие над выделенным, Alt нет; два модификатора — только `rule::TWO_MODIFIERS`. J/K, A/T/N — схема «Одиночные
/// клавиши» (S5-17).
static ALL: std::sync::LazyLock<Vec<Command>> =
    std::sync::LazyLock::new(|| crate::parts::PARTS.iter().flat_map(|p| p.commands.iter().copied()).collect());

pub(crate) fn all() -> &'static [Command] {
    &ALL
}

const fn single(command: &'static str, keys: &'static [&'static str], context: Option<&'static str>) -> SchemeKeys {
    SchemeKeys { command, keys, context }
}

/// Схема «Одиночные клавиши» (S5-17, как Gmail; DESIGN «Хоткеи», макет `SettingsKeys`): слой букв поверх клавиш
/// Outlook — только вне полей ввода; клавиши Outlook действуют и в ней. A / T / N — на приглашении без ответа (там A —
/// «Принять», а не «Ответить всем»). Виды календаря 1…4 — с видами S5-21.
pub const SINGLE: Scheme = Scheme {
    id: "single",
    keys: &[
        single("mail.next", &["j"], LIST),
        single("mail.prev", &["k"], LIST),
        single("mail.new", &["c"], LIST),
        single("mail.reply", &["r"], LIST),
        single("mail.reply_all", &["a"], LIST_NO_INVITE),
        single("mail.forward", &["f"], LIST),
        single("mail.archive", &["e"], LIST),
        single("mail.move", &["v"], LIST),
        single("mail.mark_read", &["q"], LIST),
        single("mail.mark_unread", &["u"], LIST),
        single("search.focus", &["/"], LIST),
        single("invite.accept", &["a"], INVITE),
        single("invite.tentative", &["t"], INVITE),
        single("invite.decline", &["n"], INVITE),
    ],
};

/// Привязать клавиши всех команд по выбранной схеме со своими клавишами (`prefs`, «Настройки → Клавиатура»):
/// привязки команд собираются заново на лету, чужие (gpui-kit: поля ввода и т. п.) остаются. Действие или сочетание,
/// которое не собралось, — в журнал (тесты ловят это раньше), остальные команды работают.
pub fn bind(cx: &mut App, prefs: &postman_core::prefs::Prefs) {
    let ours: Vec<String> = all().iter().map(|c| c.action_name()).collect();
    let (others, had_ours) = {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let all: Vec<KeyBinding> = keymap.bindings().cloned().collect();
        let had = all.iter().any(|b| ours.iter().any(|n| n == b.action().name()));
        (all.into_iter().filter(|b| !ours.iter().any(|n| n == b.action().name())).collect::<Vec<_>>(), had)
    };
    if had_ours {
        cx.clear_key_bindings();
        cx.bind_keys(others);
    }
    // Клавиши кнопок (подсказки, KeyTips) — по тем же привязкам (S5-18).
    crate::keytips::rebuild(cx, prefs);
    let mut bindings = Vec::new();
    let set = crate::keymap::KeySet::of(prefs);
    let features = crate::features::get(cx);
    for b in crate::keymap::bound(all(), &SINGLE, &set) {
        let Some(c) = pets_commands::find(all(), b.id) else { continue };
        let context = b.context.as_deref().and_then(|s| KeyBindingContextPredicate::parse(s).ok()).map(Rc::new);
        let keys = b.keys.as_str();
        // «Нет у сервера — нет на экране» (S7-03): клавиша команды, которой у сервера нет, — тост, а не тишина.
        let unavailable = (!crate::features::available(c.id, features)).then(|| Box::new(crate::features::Unavailable) as Box<dyn Action>);
        let action = match unavailable.or_else(|| keyed_action(c, keys)) {
            Some(a) => a,
            None => match cx.build_action(&c.action_name(), None) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("command {}: no action {} ({e})", c.id, c.action_name());
                    continue;
                }
            },
        };
        match KeyBinding::load(keys, action, context, false, None, &DummyKeyboardMapper) {
            Ok(b) => bindings.push(b),
            Err(e) => tracing::error!("command {}: keys {keys}: {e:?}", c.id),
        }
    }
    cx.bind_keys(bindings);
}

/// Действие с данными из клавиши: у `mail.categorize` клавиша — цифра категории. Остальные команды — `None`
/// (действие без данных по имени).
fn keyed_action(c: &Command, keys: &str) -> Option<Box<dyn Action>> {
    match c.id {
        "mail.categorize" => keys.parse::<u8>().ok().map(|key| Box::new(crate::actions::mail::Categorize { key }) as Box<dyn Action>),
        _ => None,
    }
}

/// Правило одного модификатора (DESIGN «Хоткеи», S5-16) — для тестов реестра; схема «Одиночные клавиши» (S5-17)
/// проверяет свои клавиши здесь же.
#[cfg(test)]
pub(crate) mod rule {
    /// Исключения правила одного модификатора (DESIGN «Хоткеи»): ради привычки Outlook и стандарта Windows.
    /// Ctrl+Shift+B — встреча (S5-23).
    pub(crate) const TWO_MODIFIERS: &[&str] = &["ctrl-shift-r", "ctrl-shift-q", "ctrl-shift-c", "ctrl-shift-b", "ctrl-shift-v", "ctrl-shift-tab"];

    /// Клавиши без модификатора (DESIGN «Хоткеи»): навигация и действие над выделенным; 1–9 — категории (решение №6);
    /// Space — «выполнено» в задачах (S6 решение 6);
    /// End — пара к Home в списке; «Меню» — контекстное меню, как Shift+F10. Буквы без модификатора — только в схеме
    /// «Одиночные клавиши» (S5-17).
    pub(crate) const BARE_KEYS: &[&str] = &[
        "up",
        "down",
        "left",
        "right",
        "enter",
        "escape",
        "delete",
        "insert",
        "f2",
        "f3",
        "home",
        "end",
        "pageup",
        "pagedown",
        "backspace",
        "menu",
        "space",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
    ];

    /// Сочетания, которые держит дизайн за будущими командами: привязать их раньше — нельзя (тест правила). Ctrl+J
    /// держали за «Взять в работу» (S5-28) — теперь это `mail.take`.
    pub(crate) const RESERVED: &[&str] = &[];

    /// Сочетание по правилу одного модификатора: `Err` — чем нарушает. Клавиши — в записи gpui, одно нажатие.
    pub(crate) fn rule_check(keys: &str) -> Result<(), String> {
        let canonical = pets_commands::parse_keys(keys)?;
        if canonical.contains(' ') {
            return Err(format!("{keys}: chords are not used"));
        }
        if TWO_MODIFIERS.iter().any(|k| pets_commands::parse_keys(k).is_ok_and(|k| k == canonical)) {
            return Ok(());
        }
        let mut parts: Vec<&str> = canonical.split('-').collect();
        // `ctrl--` — клавиша «минус».
        if parts.len() > 1 && parts.last() == Some(&"") {
            parts.pop();
            if let Some(last) = parts.last_mut() {
                *last = "-";
            }
        }
        let key = parts.pop().unwrap_or_default();
        let mods = parts;
        if mods.contains(&"alt") {
            return Err(format!("{keys}: Alt is not used"));
        }
        if mods.len() > 1 {
            return Err(format!("{keys}: more than one modifier"));
        }
        if mods.is_empty() && !BARE_KEYS.contains(&key) {
            return Err(format!("{keys}: a key without a modifier is not navigation"));
        }
        Ok(())
    }

    /// Сочетание слоя «Одиночные клавиши» (S5-17): буква или знак без модификатора — только вне полей ввода
    /// (`!Input` в контексте); остальное — по правилу схемы «Outlook».
    pub(crate) fn single_check(keys: &str, context: Option<&str>) -> Result<(), String> {
        let canonical = pets_commands::parse_keys(keys)?;
        let bare = !canonical.contains('-') || canonical == "-";
        if bare && canonical.chars().count() == 1 && !canonical.chars().all(|c| c.is_ascii_digit()) {
            return match context {
                Some(c) if c.contains("!Input") => Ok(()),
                _ => Err(format!("{keys}: a single key works only outside input fields")),
            };
        }
        rule_check(keys)
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::all;
    use gpui_kit::generate_list_of_all_registered_actions;

    #[test]
    fn the_table_is_valid() {
        assert_eq!(pets_commands::validate(all()), vec![]);
    }

    #[test]
    fn every_command_has_a_gpui_action() {
        let registered: Vec<&str> = generate_list_of_all_registered_actions().map(|a| a.name).collect();
        for c in all() {
            assert!(registered.contains(&c.action_name().as_str()), "{}: нет действия {}", c.id, c.action_name());
        }
    }

    /// `i18n/{ru,en}/commands.ftl`: у каждой команды есть название, лишних ключей `cmd-…` нет.
    #[test]
    fn every_command_has_a_name_in_both_languages() {
        for (lang, ftl) in [("ru", super::parity::RU_FTL), ("en", include_str!("../i18n/en/commands.ftl"))] {
            let names = super::parity::ftl_names(ftl);
            for c in all() {
                assert!(names.iter().any(|(k, _)| *k == c.name_key()), "{lang}.ftl: нет {}", c.name_key());
            }
            for (k, _) in names.iter().filter(|(k, _)| k.starts_with("cmd-")) {
                assert!(all().iter().any(|c| c.name_key() == *k), "{lang}.ftl: {k} — нет такой команды");
            }
        }
    }

    /// `docs/PARITY.md` — из реестра; доля сделанного не падает (это проверяется всегда).
    /// Устаревший документ PR не блокирует (R-04, скилл pets-dev): его обновляет полоса docs-gen раз в день
    /// от main — `POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui parity`.
    #[test]
    fn parity_doc_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/PARITY.md");
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        // Обработчики `on_action` — в любом модуле экрана: читаем все исходники интерфейса.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = String::new();
        for entry in std::fs::read_dir(&src).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") && path.file_name().is_some_and(|n| n != "commands.rs") {
                sources += &std::fs::read_to_string(&path).unwrap();
            }
        }
        let (doc, done) = super::parity::document(&sources);
        if let Some(was) = super::parity::done_count(&old) {
            assert!(done >= was, "сделанных функций стало меньше: было {was}, стало {done}");
        }
        if std::env::var_os("POSTMAN_UPDATE_DOCS").is_some() {
            std::fs::write(&path, &doc).expect("запись docs/PARITY.md");
        } else if old.replace("\r\n", "\n") != doc {
            eprintln!("docs/PARITY.md устарел — обновит полоса docs-gen: POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui parity");
        }
    }

    /// Клавиши писем действуют только в разделе «Почта»: в календаре и контактах Delete, j/k, Ctrl+R не трогают
    /// скрытый список писем. Создать письмо (Ctrl+N) — из любого раздела.
    #[test]
    fn mail_keys_are_scoped_to_the_mail_view() {
        for c in all().iter().filter(|c| c.area() == "mail" && c.id != "mail.new" && !c.keys.is_empty()) {
            assert!(c.context.is_some_and(|ctx| ctx.contains("MailView")), "{}: клавиши вне раздела «Почта»", c.id);
        }
        for c in all().iter().filter(|c| c.id.starts_with("calendar.") && c.id != "calendar.new_meeting") {
            assert!(c.context.is_some_and(|ctx| ctx.contains("CalendarView")), "{}: клавиши вне календаря", c.id);
        }
    }

    /// Клавиши 1–9 — категория с этой клавишей (S5-02): у каждой цифры своё действие с данными.
    #[test]
    fn digit_keys_carry_the_category_key() {
        let c = pets_commands::find(all(), "mail.categorize").unwrap();
        assert_eq!(c.keys.len(), 9);
        for (n, keys) in c.keys.iter().enumerate() {
            let a = super::keyed_action(c, keys).unwrap();
            let got = a.as_any().downcast_ref::<crate::actions::mail::Categorize>().copied();
            assert_eq!(got, Some(crate::actions::mail::Categorize { key: n as u8 + 1 }));
        }
        let next = pets_commands::find(all(), "mail.next").unwrap();
        assert!(super::keyed_action(next, "down").is_none());
    }

    /// Правило схемы «Outlook» (S5-16, DESIGN «Хоткеи») по всему реестру: не больше одного модификатора, кроме
    /// исключений; Alt нет; без модификатора — только навигация и действие над выделенным; зарезервированные
    /// сочетания свободны.
    #[test]
    fn hotkeys_follow_the_one_modifier_rule() {
        for c in all() {
            for keys in c.keys {
                assert_eq!(super::rule::rule_check(keys), Ok(()), "{}", c.id);
            }
        }
        for keys in super::rule::RESERVED {
            assert!(pets_commands::bound_to(all(), keys).is_empty(), "{keys} — за будущей командой");
        }
        // Исключения — те, что в DESIGN; каждое — два модификатора (иначе это не исключение).
        for keys in super::rule::TWO_MODIFIERS {
            assert_eq!(keys.matches('-').count(), 2, "{keys}");
        }
    }

    /// Схема «Одиночные клавиши» (S5-17): слой разобран, команды есть; буквы — только вне полей ввода; Alt нет;
    /// в слое — те буквы, что в DESIGN и макете (J/K, C, R, A, F, E, V, Q/U, `/`, A / T / N).
    #[test]
    fn single_scheme_follows_its_rule() {
        let s = &super::SINGLE;
        assert_eq!(pets_commands::validate_scheme(all(), s), vec![]);
        for k in s.keys {
            for keys in k.keys {
                assert_eq!(super::rule::single_check(keys, k.context), Ok(()), "{}", k.command);
            }
        }
        assert!(super::rule::single_check("j", Some("MailView")).is_err(), "в поле поиска J — текст");
        assert!(super::rule::single_check("alt-j", Some("MailView && !Input")).is_err());
        let layer = |id: &str| s.keys.iter().find(|k| k.command == id).map(|k| k.keys.to_vec()).unwrap_or_default();
        for (id, key) in [
            ("mail.next", "j"),
            ("mail.prev", "k"),
            ("mail.new", "c"),
            ("mail.reply", "r"),
            ("mail.reply_all", "a"),
            ("mail.forward", "f"),
            ("mail.archive", "e"),
            ("mail.move", "v"),
            ("mail.mark_read", "q"),
            ("mail.mark_unread", "u"),
            ("search.focus", "/"),
            ("invite.accept", "a"),
            ("invite.tentative", "t"),
            ("invite.decline", "n"),
        ] {
            assert_eq!(layer(id), [key], "{id}");
        }
    }

    #[test]
    fn the_rule_rejects_what_the_design_forbids() {
        use super::rule::rule_check;
        for bad in [
            "alt-s",
            "alt-up",
            "alt-home",
            "ctrl-shift-g",
            "ctrl-shift-f",
            "ctrl-alt-r",
            "j",
            "k",
            "a",
            "t",
            "n",
            "/",
            "shift-ctrl-k",
            "ctrl-k ctrl-s",
        ] {
            assert!(rule_check(bad).is_err(), "{bad} прошло правило");
        }
        for ok in [
            "ctrl-t",
            "ctrl-1",
            "ctrl-,",
            "shift-f3",
            "shift-down",
            "shift-f10",
            "insert",
            "pagedown",
            "home",
            "1",
            "shift-ctrl-r",
            "ctrl-shift-tab",
            "f2",
            "ctrl--",
        ] {
            assert_eq!(rule_check(ok), Ok(()), "{ok}");
        }
    }

    /// S5-16: клавиши, которые схема «Outlook» больше не держит, не привязаны; новые — у своих команд.
    #[test]
    fn keys_moved_by_s5_16() {
        for gone in ["j", "k", "a", "t", "n", "alt-up", "alt-down", "alt-home", "alt-s", "ctrl-shift-g", "ctrl-shift-f"] {
            assert!(pets_commands::bound_to(all(), gone).is_empty(), "{gone} всё ещё привязана");
        }
        let keys = |id: &str| pets_commands::find(all(), id).map(|c| c.keys.to_vec()).unwrap_or_default();
        assert_eq!(keys("mail.due"), ["insert"]);
        assert!(keys("mail.toggle_flag").is_empty());
        assert_eq!(keys("tab.new"), ["ctrl-t"]);
        assert_eq!(keys("calendar.prev_week"), ["pageup"]);
        assert_eq!(keys("calendar.next_week"), ["pagedown"]);
        assert_eq!(keys("calendar.today"), ["home"]);
        assert_eq!(keys("compose.send"), ["ctrl-enter"]);
        assert_eq!(keys("meeting.send"), ["ctrl-enter"]);
    }

    /// Хоткеи classic Outlook из `docs/HANDOFF.md` §2a.
    #[test]
    fn outlook_hotkeys_are_bound() {
        for keys in ["ctrl-n", "ctrl-r", "ctrl-shift-r", "ctrl-f", "ctrl-e", "ctrl-1", "ctrl-2", "ctrl-3", "delete", "ctrl-q", "ctrl-u"] {
            assert!(!pets_commands::bound_to(all(), keys).is_empty(), "хоткей Outlook {keys} не привязан");
        }
    }
}

/// Чек-лист соответствия Outlook (S2-04): `docs/PARITY.md` из реестра команд.
#[cfg(test)]
pub(crate) mod parity {
    use super::all;

    pub const RU_FTL: &str = include_str!("../i18n/ru/commands.ftl");

    /// Пары `ключ = значение` из `.ftl` (только простые сообщения в одну строку).
    pub fn ftl_names(ftl: &str) -> Vec<(&str, &str)> {
        ftl.lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with(' '))
            .filter_map(|l| l.split_once(" = "))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect()
    }

    /// Действия, у которых в коде экрана есть обработчик: `on_action(… _: &mail::Next …)` или с данными
    /// (`a: &mail::Categorize`). Прочие `x: &Тип` с именами действий не совпадают.
    fn handled(source: &str) -> Vec<String> {
        source
            .split(": &")
            .skip(1)
            .filter_map(|rest| rest.split([',', ')']).next())
            .map(|path| {
                let parts: Vec<&str> = path.trim().split("::").collect();
                parts[parts.len().saturating_sub(2)..].join("::")
            })
            .collect()
    }

    /// Число сделанных функций из прошлой версии документа (`Сделано: N из M`).
    pub fn done_count(doc: &str) -> Option<usize> {
        let rest = doc.split("Сделано: ").nth(1)?;
        rest.split_whitespace().next()?.parse().ok()
    }

    /// Текст документа и число сделанных функций.
    pub fn document(screen_source: &str) -> (String, usize) {
        let names = ftl_names(RU_FTL);
        let handled = handled(screen_source);
        let done = all().iter().filter(|c| handled.contains(&c.action_name())).count();
        let mut s = String::new();
        s.push_str("# Соответствие classic Outlook\n\n");
        s.push_str("> Генерируется из реестра команд (`app-gpui/src/commands.rs`) тестом `parity_doc_is_current`.\n");
        s.push_str("> Руками не править. Обновить: `POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui parity`.\n");
        s.push_str("> «есть» — у действия команды есть обработчик на экране; доля сделанного только растёт (тест).\n\n");
        s.push_str(&format!("Сделано: {done} из {} ({} %).\n\n", all().len(), done * 100 / all().len().max(1)));
        s.push_str("Хоткей — схема «Outlook» (по умолчанию); «Одиночные» — буквы схемы «Одиночные клавиши» (S5-17) поверх неё.\n\n");
        s.push_str("| Функция | Команда | Хоткей | Одиночные | Статус |\n|---|---|---|---|---|\n");
        for c in all() {
            let key = c.name_key();
            let name = names.iter().find(|(k, _)| *k == key).map_or(key.as_str(), |(_, v)| v);
            let keys: Vec<String> = c.keys.iter().map(|k| format!("`{k}`")).collect();
            let single: Vec<String> =
                super::SINGLE.keys.iter().filter(|k| k.command == c.id).flat_map(|k| k.keys).map(|k| format!("`{k}`")).collect();
            let status = if handled.contains(&c.action_name()) { "есть" } else { "нет" };
            s.push_str(&format!("| {name} | `{}` | {} | {} | {status} |\n", c.id, keys.join(", "), single.join(", ")));
        }
        (s, done)
    }
}
