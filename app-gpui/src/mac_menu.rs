//! Строка меню macOS (MAC-02, MAC-07): состав — таблица [`MENUS`], она собирается на всех ОС (тест на Windows), меню
//! gpui из неё строит `mac_app` (только macOS). Пункты команд — из реестра (`commands.rs`): то же действие, клавиша в
//! меню — из той же привязки (`commands::bind` ставит меню заново после привязки клавиш). Внутреннее меню окна
//! (`menu.rs`) — своё, это его не заменяет.

// Вне macOS таблицу читает только тест.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// Системное действие macOS — у `mac_app`, в реестре команд его нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sys {
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    Minimize,
    Zoom,
    FullScreen,
}

/// Правка текста — действие поля ввода gpui-kit, у системы — свой пункт (`OsAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edit {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Пункт меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    /// Команда реестра (`id`); подпись — своя (`mac-menu-…`) или, если `None`, название команды.
    Command(&'static str, Option<&'static str>),
    /// Системное действие и подпись.
    Sys(Sys, &'static str),
    /// Правка текста и подпись.
    Edit(Edit, &'static str),
    /// Меню «Службы» системы.
    Services(&'static str),
    Separator,
}

/// Меню строки меню: заголовок (`None` — имя программы) и пункты.
pub(crate) struct Menu {
    pub title: Option<&'static str>,
    pub items: &'static [Item],
}

use Item::{Command, Separator};

/// Строка меню, как принято на Mac: программа, «Файл», «Правка», «Вид», «Окно». «О программе» нет: своего экрана
/// у Postman нет.
pub(crate) const MENUS: &[Menu] = &[
    Menu {
        title: None,
        items: &[
            Command("settings.open", Some("mac-menu-settings")),
            Command("update.check", None),
            Separator,
            Item::Services("mac-menu-services"),
            Separator,
            Item::Sys(Sys::Hide, "mac-menu-hide"),
            Item::Sys(Sys::HideOthers, "mac-menu-hide-others"),
            Item::Sys(Sys::ShowAll, "mac-menu-show-all"),
            Separator,
            Item::Sys(Sys::Quit, "mac-menu-quit"),
        ],
    },
    Menu {
        title: Some("mac-menu-file"),
        items: &[
            Command("mail.new", None),
            Command("calendar.new_meeting", Some("mac-menu-new-meeting")),
            Separator,
            Command("tab.new", None),
            Command("tab.close", None),
            Separator,
            Command("mail.export", None),
        ],
    },
    Menu {
        title: Some("mac-menu-edit"),
        items: &[
            Item::Edit(Edit::Undo, "mac-menu-undo"),
            Item::Edit(Edit::Redo, "mac-menu-redo"),
            Separator,
            Item::Edit(Edit::Cut, "mac-menu-cut"),
            Item::Edit(Edit::Copy, "mac-menu-copy"),
            Item::Edit(Edit::Paste, "mac-menu-paste"),
            Item::Edit(Edit::SelectAll, "mac-menu-select-all"),
            Separator,
            Command("search.focus", None),
        ],
    },
    Menu {
        title: Some("mac-menu-view"),
        items: &[
            Command("view.mail", None),
            Command("view.calendar", None),
            Command("view.contacts", None),
            Command("view.tasks", None),
            Separator,
            Command("palette.open", None),
        ],
    },
    Menu {
        title: Some("mac-menu-window"),
        items: &[
            Item::Sys(Sys::Minimize, "mac-menu-minimize"),
            Item::Sys(Sys::Zoom, "mac-menu-zoom"),
            Item::Sys(Sys::FullScreen, "mac-menu-fullscreen"),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::{Item, MENUS};
    use crate::commands::parity::ftl_names;

    /// У каждого пункта строки меню macOS есть команда в реестре и подпись на обоих языках; у меню — заголовок.
    #[test]
    fn mac_menu_items_have_commands_and_texts() {
        let langs = [
            ("ru", include_str!("../i18n/ru/frame.ftl"), include_str!("../i18n/ru/commands.ftl")),
            ("en", include_str!("../i18n/en/frame.ftl"), include_str!("../i18n/en/commands.ftl")),
        ];
        for (lang, frame, commands) in langs {
            let (frame, commands) = (ftl_names(frame), ftl_names(commands));
            let has = |names: &[(&str, &str)], key: &str| names.iter().any(|(k, v)| *k == key && !v.is_empty());
            for menu in MENUS {
                if let Some(title) = menu.title {
                    assert!(has(&frame, title), "{lang}: нет заголовка меню {title}");
                }
                for item in menu.items {
                    let label = match *item {
                        Item::Command(id, label) => {
                            let c = pets_commands::find(crate::commands::all(), id);
                            let c = c.unwrap_or_else(|| panic!("пункт меню {id}: нет команды в реестре"));
                            match label {
                                Some(label) => label,
                                None => {
                                    assert!(has(&commands, &c.name_key()), "{lang}: нет названия {}", c.name_key());
                                    continue;
                                }
                            }
                        }
                        Item::Sys(_, label) | Item::Edit(_, label) | Item::Services(label) => label,
                        Item::Separator => continue,
                    };
                    assert!(label.starts_with("mac-menu-"), "{label}: подписи меню — ключи mac-menu-*");
                    assert!(has(&frame, label), "{lang}: нет подписи {label}");
                }
            }
        }
    }
}
