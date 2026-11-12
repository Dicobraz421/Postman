//! Значок в трее (T3-13, решение 2026-11-12 «tray-and-notifications»): пока Postman запущен — значок в области
//! уведомлений; закрытие окна прячет его в трей, синхронизация и уведомления живут. Щелчок по значку или уведомлению
//! Windows возвращает окно. Значок — только Windows: на macOS окно прячет система (`mac_app`), а щелчок по уведомлению
//! возвращает окно и открывает письмо ([`mac`], MAC-04); на остальных ОС значка нет.
//!
//! Значок и уведомление сообщают о щелчке не из задачи интерфейса (оконная процедура значка, поток Windows) —
//! событие уходит каналом ([`send`]) в задачу интерфейса.

use std::sync::OnceLock;

use futures::channel::mpsc::UnboundedSender;
use gpui_kit::*;
use pets_i18n::tr;
use postman_core::newmail::Fresh;

use crate::Mailer;

/// Что просит значок или уведомление.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Показать окно.
    Open,
    /// Выйти из Postman.
    Quit,
    /// Показать окно и письмо; `None` — «Входящие».
    Letter(Option<String>),
}

#[cfg_attr(not(windows), allow(dead_code))]
static TX: OnceLock<UnboundedSender<Signal>> = OnceLock::new();

/// Передать событие задаче интерфейса; значка нет — никуда.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn send(signal: Signal) {
    if let Some(tx) = TX.get() {
        let _ = tx.unbounded_send(signal);
    }
}

/// Подсказка значка: непрочитанные «Входящих».
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn tip(unread: u32) -> String {
    if unread == 0 { "Postman".to_owned() } else { tr!("tray-tip-unread", n = unread) }
}

/// Куда ведёт щелчок по уведомлению о новых письмах: одно письмо — оно, несколько — «Входящие».
pub(crate) fn target(list: &[Fresh]) -> Option<String> {
    match list {
        [one] => Some(one.id.clone()),
        _ => None,
    }
}

#[cfg(windows)]
mod os {
    use futures::StreamExt as _;
    use gpui_windows::pets::tray::{Event, Item, Tray};
    use postman_core::prefs::OnClose;

    use super::*;

    /// Значки в ресурсах exe (`build.rs`): обычный и с отметкой «есть непрочитанные».
    const ICON: u16 = 1;
    const ICON_UNREAD: u16 = 2;
    const OPEN: u32 = 1;
    const QUIT: u32 = 2;

    struct Icon {
        tray: Tray,
        unread: u32,
    }

    impl Global for Icon {}

    pub(crate) fn init(window: &mut Window, mailer: &Entity<Mailer>, cx: &mut App) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        if TX.set(tx).is_err() {
            return;
        }
        let on_event = |e| {
            send(match e {
                Event::Menu(QUIT) => Signal::Quit,
                Event::Click | Event::Menu(_) => Signal::Open,
            })
        };
        let tray = match Tray::new(ICON, &tip(0), on_event) {
            Ok(tray) => tray,
            Err(e) => {
                // Без значка окно закрывается, как раньше: спрятанное было бы не вернуть.
                tracing::warn!("tray: tray icon not created, the window closes as usual: {e}");
                return;
            }
        };
        tray.set_menu(vec![Item::Command(OPEN, tr!("tray-open")), Item::Separator, Item::Command(QUIT, tr!("tray-quit"))]);
        cx.set_global(Icon { tray, unread: 0 });
        window.on_window_should_close(cx, |window, cx| crate::prefs::get(cx).general.on_close != OnClose::Tray || !hide(window, true, cx));
        let folders = mailer.read(cx).folders.clone();
        cx.observe(&folders, |folders, cx| {
            let unread: u32 = folders.read(cx).of(None).filter(|m| m.role.as_deref() == Some("inbox")).map(|m| m.unread).sum();
            set_unread(unread, cx);
        })
        .detach();
        let handle = window.window_handle();
        let mailer = mailer.clone();
        cx.spawn(async move |cx| {
            while let Some(signal) = rx.next().await {
                let _ = handle.update(cx, |_, window, cx| match signal {
                    Signal::Quit => cx.quit(),
                    Signal::Open => show(window),
                    Signal::Letter(id) => {
                        show(window);
                        mailer.update(cx, |m, cx| m.open_new_mail(id, cx));
                    }
                });
            }
        })
        .detach();
    }

    fn set_unread(unread: u32, cx: &mut App) {
        let icon = cx.global_mut::<Icon>();
        if icon.unread == unread {
            return;
        }
        let flipped = (icon.unread == 0) != (unread == 0);
        icon.unread = unread;
        let mut result = icon.tray.set_tip(&tip(unread));
        if flipped {
            result = result.and(icon.tray.set_icon(if unread == 0 { ICON } else { ICON_UNREAD }));
        }
        if let Err(e) = result {
            tracing::warn!("tray: tray icon not updated: {e}");
        }
    }

    pub(crate) fn hide(window: &mut Window, hint: bool, cx: &mut App) -> bool {
        if !cx.has_global::<Icon>() {
            return false;
        }
        if let Err(e) = crate::sys::hide_window(window) {
            tracing::error!("tray: window not hidden: {e}");
            return false;
        }
        if hint && !crate::prefs::get(cx).general.tray_hint_shown {
            crate::prefs::update(cx, |p| p.general.tray_hint_shown = true);
            let (title, body) = (tr!("tray-hint-title"), tr!("tray-hint-body"));
            // Показ — вызов COM в другой процесс: не в потоке окна.
            cx.background_spawn(async move {
                if let Err(e) = crate::screens::notify(&title, &body) {
                    tracing::warn!("tray: tray hint notification not shown: {e}");
                }
            })
            .detach();
        }
        true
    }

    pub(crate) fn show(window: &mut Window) {
        if let Err(e) = crate::sys::show_window(window) {
            tracing::error!("tray: window not shown: {e}");
        }
        window.activate_window();
    }
}

/// macOS (MAC-04): значка нет, окно скрывает система; щелчок по уведомлению — вернуть окно и открыть письмо.
#[cfg(target_os = "macos")]
mod mac {
    use futures::StreamExt as _;

    use super::*;

    pub(crate) fn init(window: &mut Window, mailer: &Entity<Mailer>, cx: &mut App) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        if TX.set(tx).is_err() {
            return;
        }
        let handle = window.window_handle();
        let mailer = mailer.clone();
        cx.spawn(async move |cx| {
            while let Some(signal) = rx.next().await {
                let _ = handle.update(cx, |_, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                    match signal {
                        Signal::Open => {}
                        Signal::Quit => cx.quit(),
                        Signal::Letter(id) => mailer.update(cx, |m, cx| m.open_new_mail(id, cx)),
                    }
                });
            }
        })
        .detach();
    }
}

/// Поставить значок и прятать окно при закрытии («Настройки → Общие → Закрытие окна»). Не удалось — окно
/// закрывается, как раньше. macOS — только щелчок по уведомлению.
pub(crate) fn init(window: &mut Window, mailer: &Entity<Mailer>, cx: &mut App) {
    #[cfg(windows)]
    os::init(window, mailer, cx);
    #[cfg(target_os = "macos")]
    mac::init(window, mailer, cx);
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = (window, mailer, cx);
}

/// Спрятать окно в трей; `hint` — при первом разе сказать об этом уведомлением Windows. `false` — значка нет, окно
/// осталось.
pub(crate) fn hide(window: &mut Window, hint: bool, cx: &mut App) -> bool {
    #[cfg(windows)]
    return os::hide(window, hint, cx);
    #[cfg(not(windows))]
    {
        let _ = (window, hint, cx);
        false
    }
}

/// Показать спрятанное или свёрнутое окно и вывести вперёд.
pub(crate) fn show(window: &mut Window) {
    #[cfg(windows)]
    os::show(window);
    #[cfg(not(windows))]
    window.activate_window();
}

#[cfg(test)]
mod tests {
    use super::{Fresh, target, tip};

    fn f(n: usize) -> Fresh {
        Fresh { id: n.to_string(), from: String::new(), subject: String::new(), received_at: 0, important: false }
    }

    /// Щелчок по уведомлению: одно письмо — оно, несколько — «Входящие»; подсказка без непрочитанных — имя.
    #[test]
    fn tray_click_target_and_tip() {
        assert_eq!(target(&[f(7)]), Some("7".to_owned()));
        assert_eq!(target(&[f(1), f(2)]), None);
        assert_eq!(tip(0), "Postman");
    }
}
