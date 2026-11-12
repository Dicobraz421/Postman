//! macOS (MAC-02): меню приложения и системные клавиши, как принято на Mac. Своего меню в окне (`menu.rs`) это не
//! заменяет: здесь — только то, что macOS ждёт от любого приложения в строке меню и на клавишах.
//!
//! - ⌘Q — выход, ⌘H — скрыть, ⌥⌘H — скрыть остальные, ⌘M — свернуть окно, ⌃⌘F — во весь экран.
//! - Красная кнопка и ⌘W на главной вкладке (`tab.close` в `chrome.rs`, MAC-04) и в окне без вкладок скрывают
//!   приложение, а не закрывают окно: почта продолжает
//!   синхронизироваться и показывать уведомления, щелчок по значку в Dock возвращает окно (как «Почта» Apple).
//! - Ссылки `mailto:` и щелчок по Dock приходят первому экземпляру событием Apple — `main.rs`.

use gpui_kit::*;
use pets_i18n::tr;

gpui_kit::actions!(mac, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom, FullScreen, CloseWindow]);

/// Системные клавиши и меню приложения. Вызывать после `gpui_kit::init` и до `commands::bind` (тот оставляет чужие
/// привязки как есть).
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
    cx.set_menus([
        Menu::new("Postman").items([
            MenuItem::os_submenu(tr!("mac-menu-services"), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(tr!("mac-menu-hide"), Hide),
            MenuItem::action(tr!("mac-menu-hide-others"), HideOthers),
            MenuItem::action(tr!("mac-menu-show-all"), ShowAll),
            MenuItem::separator(),
            MenuItem::action(tr!("mac-menu-quit"), Quit),
        ]),
        Menu::new(tr!("mac-menu-window")).items([
            MenuItem::action(tr!("mac-menu-minimize"), Minimize),
            MenuItem::action(tr!("mac-menu-zoom"), Zoom),
            MenuItem::action(tr!("mac-menu-fullscreen"), FullScreen),
        ]),
    ]);
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
