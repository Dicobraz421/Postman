//! macOS (MAC-02): меню приложения и системные клавиши, как принято на Mac. Своего меню в окне (`menu.rs`) это не
//! заменяет: здесь — только то, что macOS ждёт от любого приложения в строке меню и на клавишах.
//!
//! - ⌘Q — выход, ⌘H — скрыть, ⌥⌘H — скрыть остальные, ⌘M — свернуть окно, ⌃⌘F — во весь экран.
//! - Красная кнопка и ⌘W на главной вкладке (`tab.close` в `chrome.rs`, MAC-04) и в окне без вкладок скрывают
//!   приложение, а не закрывают окно: почта продолжает
//!   синхронизироваться и показывать уведомления, щелчок по значку в Dock возвращает окно (как «Почта» Apple).
//! - Ссылки `mailto:` и щелчок по Dock приходят первому экземпляру событием Apple — `main.rs`.
//! - Строка меню (MAC-07) — по таблице `mac_menu::MENUS`: команды реестра, правка текста, системные пункты.

use gpui_kit::component::input;
use gpui_kit::*;
use pets_i18n::tr;

use crate::mac_menu::{self, Edit, Item, Sys};

gpui_kit::actions!(mac, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom, FullScreen, CloseWindow]);

/// Системные клавиши. Вызывать после `gpui_kit::init` и до `commands::bind` (тот оставляет чужие привязки как есть и
/// ставит строку меню — [`set_menus`]).
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| with_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &FullScreen, cx| with_window(cx, |w| w.toggle_fullscreen()));
    cx.on_action(|_: &CloseWindow, cx| cx.hide());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", FullScreen, None),
        // ⌘W вкладки (`tab.close`, контекст `Mailer`) глубже и срабатывает первой; это — для окна без вкладок.
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
}

/// Строка меню по таблице `mac_menu::MENUS`. Клавиши пунктов система берёт из привязок в момент вызова — звать после
/// каждой привязки клавиш (`commands::bind`).
pub(crate) fn set_menus(cx: &App) {
    let menus: Vec<Menu> = mac_menu::MENUS
        .iter()
        .map(|m| {
            let title: SharedString = m.title.map_or_else(|| "Postman".into(), |k| tr!(k).into());
            Menu::new(title).items(m.items.iter().filter_map(|item| menu_item(item, cx)))
        })
        .collect();
    cx.set_menus(menus);
}

/// Пункт меню gpui; команды без действия — в журнал и без пункта (тест реестра ловит это раньше).
fn menu_item(item: &Item, cx: &App) -> Option<MenuItem> {
    Some(match *item {
        Item::Command(id, label) => {
            let c = pets_commands::find(crate::commands::all(), id)?;
            let action = match cx.build_action(&c.action_name(), None) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(command = %id, "mac menu: no action {} ({e})", c.action_name());
                    return None;
                }
            };
            let name = label.map_or_else(|| tr!(&c.name_key()), |k| tr!(k));
            MenuItem::Action { name: name.into(), action, os_action: None, checked: false, disabled: false }
        }
        Item::Sys(sys, label) => {
            let name = tr!(label);
            match sys {
                Sys::Hide => MenuItem::action(name, Hide),
                Sys::HideOthers => MenuItem::action(name, HideOthers),
                Sys::ShowAll => MenuItem::action(name, ShowAll),
                Sys::Quit => MenuItem::action(name, Quit),
                Sys::Minimize => MenuItem::action(name, Minimize),
                Sys::Zoom => MenuItem::action(name, Zoom),
                Sys::FullScreen => MenuItem::action(name, FullScreen),
            }
        }
        Item::Edit(edit, label) => {
            let name = tr!(label);
            match edit {
                Edit::Undo => MenuItem::os_action(name, input::Undo, OsAction::Undo),
                Edit::Redo => MenuItem::os_action(name, input::Redo, OsAction::Redo),
                Edit::Cut => MenuItem::os_action(name, input::Cut, OsAction::Cut),
                Edit::Copy => MenuItem::os_action(name, input::Copy, OsAction::Copy),
                Edit::Paste => MenuItem::os_action(name, input::Paste, OsAction::Paste),
                Edit::SelectAll => MenuItem::os_action(name, input::SelectAll, OsAction::SelectAll),
            }
        }
        Item::Services(label) => MenuItem::os_submenu(tr!(label), SystemMenuType::Services),
        Item::Separator => MenuItem::separator(),
    })
}

/// Действие над окном впереди (у системных действий своего окна нет).
fn with_window(cx: &mut App, f: impl FnOnce(&mut Window)) {
    if let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) {
        let _ = handle.update(cx, |_, window, _| f(window));
    }
}

/// Главное окно: красная кнопка скрывает приложение (окно, почта и уведомления живут), ⌘Q — выход.
pub(crate) fn keep_main_window(window: &mut Window, cx: &mut App) {
    window.on_window_should_close(cx, |_, cx| {
        cx.hide();
        false
    });
}

/// Щелчок по значку в Dock при работающем приложении: вернуть окно.
pub(crate) fn reopen(cx: &mut App) {
    cx.activate(true);
    if let Some(handle) = cx.windows().first().copied() {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
}
