//! Значок в трее (T3-13, решение 2026-11-12 «tray-and-notifications»): пока Postman запущен — значок в области
//! уведомлений; закрытие окна прячет его в трей, синхронизация и уведомления живут. Щелчок по значку или уведомлению
//! Windows возвращает окно. Значок — только Windows: на macOS окно прячет система (`mac_app`), а щелчок по уведомлению
//! возвращает окно и открывает письмо ([`mac`], MAC-04); непрочитанные «Входящих» там — бейдж на значке в Dock
//! (MAC-07, [`badge`]) и число рядом со значком в строке меню (MAC-09, [`menu_bar_title`]): у значка меню
//! [`menu_bar_menu`], убрать его можно настройкой «Значок в строке меню». Linux (PAR-06) — значок
//! StatusNotifierItem, как на Windows; нет хоста значков — значка нет.
//!
//! Значок и уведомление сообщают о щелчке не из задачи интерфейса (оконная процедура значка, поток Windows) —
//! событие уходит каналом ([`send`]) в задачу интерфейса.

use std::sync::OnceLock;

use futures::channel::mpsc::UnboundedSender;
use gpui_kit::*;
use pets_i18n::tr;
use postman_core::newmail::Fresh;

use crate::Mailer;
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
use crate::folders::Folders;

/// Что просит значок или уведомление.
#[cfg_attr(not(windows), allow(dead_code, reason = "not every OS sends every signal"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Показать окно.
    Open,
    /// Выйти из Postman.
    Quit,
    /// Показать окно и письмо; `None` — «Входящие».
    Letter(Option<String>),
    /// Показать окно и новое письмо (команда `mail.new`; меню значка в строке меню, MAC-09).
    Compose,
}

#[cfg_attr(not(any(windows, target_os = "macos", target_os = "linux")), allow(dead_code, reason = "no tray icon on this OS"))]
static TX: OnceLock<UnboundedSender<Signal>> = OnceLock::new();

/// Передать событие задаче интерфейса; значка нет — никуда.
#[cfg_attr(not(any(windows, target_os = "macos", target_os = "linux")), allow(dead_code, reason = "no tray icon on this OS"))]
pub(crate) fn send(signal: Signal) {
    if let Some(tx) = TX.get() {
        let _ = tx.unbounded_send(signal);
    }
}

/// Подсказка значка: непрочитанные «Входящих».
#[cfg_attr(not(any(windows, target_os = "macos", target_os = "linux")), allow(dead_code, reason = "no tray icon on this OS"))]
pub(crate) fn tip(unread: u32) -> String {
    if unread == 0 { "Postman".to_owned() } else { tr!("tray-tip-unread", n = unread) }
}

/// Бейдж на значке в Dock (MAC-07): непрочитанные «Входящих»; нет непрочитанных — без бейджа, больше 999 — «999+»
/// (бейдж узкий).
#[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "Dock badge is macOS only; tested on every OS"))]
pub(crate) fn badge(unread: u32) -> Option<String> {
    match unread {
        0 => None,
        1..=999 => Some(unread.to_string()),
        _ => Some("999+".to_owned()),
    }
}

/// Число рядом со значком в строке меню (MAC-09) — как бейдж в Dock: нет непрочитанных — только значок, больше 999
/// — «999+».
#[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "menu bar icon is macOS only; tested on every OS"))]
pub(crate) fn menu_bar_title(unread: u32) -> Option<String> {
    badge(unread)
}

/// Пункт меню значка в строке меню: что просит и ключ текста; `None` — разделитель.
#[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "menu bar icon is macOS only; tested on every OS"))]
pub(crate) type MenuBarEntry = Option<(Signal, &'static str)>;

/// Меню значка в строке меню (MAC-09): «Открыть Postman», «Новое письмо», разделитель, «Выйти». Номер пункта для
/// `pets_gpui_macos::status_item` — его место в списке.
#[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "menu bar icon is macOS only; tested on every OS"))]
pub(crate) fn menu_bar_menu() -> [MenuBarEntry; 4] {
    [Some((Signal::Open, "tray-open")), Some((Signal::Compose, "frame-new")), None, Some((Signal::Quit, "menubar-quit"))]
}

/// Непрочитанные «Входящих» всех учётных записей — подсказка значка в трее и бейдж в Dock.
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn inbox_unread(folders: &Entity<Folders>, cx: &App) -> u32 {
    folders.read(cx).of(None).filter(|m| m.role.as_deref() == Some("inbox")).map(|m| m.unread).sum()
}

/// Куда ведёт щелчок по уведомлению о новых письмах: одно письмо — оно, несколько — «Входящие».
pub(crate) fn target(list: &[Fresh]) -> Option<String> {
    match list {
        [one] => Some(one.id.clone()),
        _ => None,
    }
}

/// Windows — `gpui_windows::pets::tray` (область уведомлений); Linux (PAR-06) — StatusNotifierItem
/// (`sys::tray`, тот же API): значок, подсказка с непрочитанными, меню «Открыть»/«Выход», закрытие окна — в трей.
#[cfg(any(windows, target_os = "linux"))]
mod os {
    use futures::StreamExt as _;
    #[cfg(windows)]
    use gpui_windows::pets::tray::{Event, Item, Tray};
    use postman_core::prefs::OnClose;

    #[cfg(target_os = "linux")]
    use crate::sys::tray::{Event, Item, Tray};

    use super::*;

    /// Значки в ресурсах exe (`build.rs`): обычный и с отметкой «есть непрочитанные».
    #[cfg(windows)]
    const ICON: u16 = 1;
    #[cfg(windows)]
    const ICON_UNREAD: u16 = 2;
    /// Linux: номер значка `sys::tray` (те же `.ico`).
    #[cfg(target_os = "linux")]
    const ICON: u16 = 0;
    #[cfg(target_os = "linux")]
    const ICON_UNREAD: u16 = 1;
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
        // Сигналы — и без значка (PAR-06): щелчок по уведомлению открывает письмо, даже если хоста значков нет.
        let handle = window.window_handle();
        let events_mailer = mailer.clone();
        cx.spawn(async move |cx| {
            while let Some(signal) = rx.next().await {
                let _ = handle.update(cx, |_, window, cx| match signal {
                    Signal::Quit => cx.quit(),
                    Signal::Open => show(window),
                    Signal::Letter(id) => {
                        show(window);
                        events_mailer.update(cx, |m, cx| m.open_new_mail(id, cx));
                    }
                    Signal::Compose => {
                        show(window);
                        events_mailer.update(cx, |m, cx| m.open_compose(Default::default(), cx));
                    }
                });
            }
        })
        .detach();
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
            let unread = inbox_unread(&folders, cx);
            set_unread(unread, cx);
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

/// macOS (MAC-04): значка в трее нет, окно скрывает система; щелчок по уведомлению — вернуть окно и открыть письмо.
/// Непрочитанные «Входящих» — бейдж на значке в Dock (MAC-07) и число рядом со значком в строке меню (MAC-09), там
/// же, где Windows меняет подсказку значка. Значок в строке меню — по настройке «Значок в строке меню»: настройки
/// изменились — значок ставится или убирается сразу; сменился язык — меню собирается заново.
#[cfg(target_os = "macos")]
mod mac {
    use futures::StreamExt as _;
    use pets_gpui_macos::status_item::{Item, StatusItem};

    use super::*;

    /// Значок-шаблон «Ленты» (`postman.svg` по сетке 36 px — 18 точек вдвое): система берёт только прозрачность и
    /// красит его под строку меню.
    const ICON: &[u8] = include_bytes!("../res/menubar.png");

    /// Значок в строке меню и что на нём показано.
    #[derive(Default)]
    struct MenuBar {
        /// `None` — настройка выключена или значок не поставлен; `Drop` убирает значок.
        item: Option<StatusItem>,
        unread: u32,
        /// Язык, на котором собрано меню.
        lang: Option<String>,
    }

    impl Global for MenuBar {}

    pub(crate) fn init(window: &mut Window, mailer: &Entity<Mailer>, cx: &mut App) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        if TX.set(tx).is_err() {
            return;
        }
        let folders = mailer.read(cx).folders.clone();
        let mut shown = 0;
        let unread = inbox_unread(&folders, cx);
        set_badge(&mut shown, unread);
        cx.set_global(MenuBar::default());
        sync_menu_bar(cx);
        set_unread(unread, cx);
        cx.observe(&folders, move |folders, cx| {
            let unread = inbox_unread(&folders, cx);
            set_badge(&mut shown, unread);
            set_unread(unread, cx);
        })
        .detach();
        crate::prefs::observe_app(cx, sync_menu_bar).detach();
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
                        Signal::Compose => mailer.update(cx, |m, cx| m.open_compose(Default::default(), cx)),
                    }
                });
            }
        })
        .detach();
    }

    /// Бейдж в Dock — только когда число сменилось.
    fn set_badge(shown: &mut u32, unread: u32) {
        if *shown == unread {
            return;
        }
        *shown = unread;
        if let Err(e) = crate::sys::set_dock_badge(badge(unread).as_deref()) {
            tracing::warn!("tray: dock badge not updated: {e}");
        }
    }

    /// Число и подсказка значка в строке меню — только когда число сменилось.
    fn set_unread(unread: u32, cx: &mut App) {
        let bar = cx.global_mut::<MenuBar>();
        if bar.unread == unread {
            return;
        }
        bar.unread = unread;
        if let Some(item) = &bar.item {
            item.set_title(menu_bar_title(unread).as_deref());
            item.set_tip(&tip(unread));
        }
    }

    /// Значок в строке меню по настройке: включена — поставить (или собрать меню заново на новом языке), выключена —
    /// убрать.
    fn sync_menu_bar(cx: &mut App) {
        let (on, lang) = crate::prefs::with(cx, |p| (p.general.menu_bar_icon, p.general.language.clone()));
        let bar = cx.global_mut::<MenuBar>();
        if !on {
            bar.item = None;
            return;
        }
        if bar.item.is_some() && bar.lang == lang {
            return;
        }
        if bar.item.is_none() {
            let on_menu = |id: u32| {
                if let Some(Some((signal, _))) = menu_bar_menu().get(id as usize) {
                    send(signal.clone());
                }
            };
            match StatusItem::new(ICON, &tip(bar.unread), on_menu) {
                Ok(item) => {
                    item.set_title(menu_bar_title(bar.unread).as_deref());
                    bar.item = Some(item);
                }
                Err(e) => {
                    tracing::warn!("tray: menu bar icon not created: {e}");
                    return;
                }
            }
        }
        bar.lang = lang;
        let items: Vec<Item> = menu_bar_menu()
            .iter()
            .enumerate()
            .map(|(id, entry)| match entry {
                Some((_, key)) => Item::Command(id as u32, tr!(key)),
                None => Item::Separator,
            })
            .collect();
        if let Some(item) = &bar.item {
            item.set_menu(&items);
            item.set_tip(&tip(bar.unread));
        }
    }
}

/// Поставить значок и прятать окно при закрытии («Настройки → Общие → Закрытие окна»). Не удалось — окно
/// закрывается, как раньше. macOS — только щелчок по уведомлению.
pub(crate) fn init(window: &mut Window, mailer: &Entity<Mailer>, cx: &mut App) {
    #[cfg(any(windows, target_os = "linux"))]
    os::init(window, mailer, cx);
    #[cfg(target_os = "macos")]
    mac::init(window, mailer, cx);
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let _ = (window, mailer, cx);
}

/// Linux (PAR-06): своя кнопка «✕» — как закрытие окна системой (`on_window_should_close` её не видит): по настройке
/// «Закрытие окна» спрятать в трей; `true` — спрятано, окно не закрывать.
#[cfg(target_os = "linux")]
pub(crate) fn close_to_tray(window: &mut Window, cx: &mut App) -> bool {
    crate::prefs::get(cx).general.on_close == postman_core::prefs::OnClose::Tray && hide(window, true, cx)
}

/// Спрятать окно в трей; `hint` — при первом разе сказать об этом уведомлением Windows. `false` — значка нет, окно
/// осталось.
pub(crate) fn hide(window: &mut Window, hint: bool, cx: &mut App) -> bool {
    #[cfg(any(windows, target_os = "linux"))]
    return os::hide(window, hint, cx);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (window, hint, cx);
        false
    }
}

/// Показать спрятанное или свёрнутое окно и вывести вперёд.
pub(crate) fn show(window: &mut Window) {
    #[cfg(any(windows, target_os = "linux"))]
    os::show(window);
    #[cfg(not(any(windows, target_os = "linux")))]
    window.activate_window();
}

#[cfg(test)]
mod tests {
    use super::{Fresh, Signal, badge, menu_bar_menu, menu_bar_title, target, tip};

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

    /// Бейдж в Dock (MAC-07): 0 — нет, 1…999 — число, больше — «999+».
    #[test]
    fn dock_badge_text() {
        assert_eq!(badge(0), None);
        assert_eq!(badge(1).as_deref(), Some("1"));
        assert_eq!(badge(999).as_deref(), Some("999"));
        assert_eq!(badge(1000).as_deref(), Some("999+"));
        assert_eq!(badge(u32::MAX).as_deref(), Some("999+"));
    }

    /// Значок в строке меню (MAC-09): число — как бейдж Dock (0 — пусто, больше 999 — «999+»).
    #[test]
    fn menu_bar_count_text() {
        assert_eq!(menu_bar_title(0), None);
        assert_eq!(menu_bar_title(7).as_deref(), Some("7"));
        assert_eq!(menu_bar_title(999).as_deref(), Some("999"));
        assert_eq!(menu_bar_title(1000).as_deref(), Some("999+"));
    }

    /// Меню значка в строке меню (MAC-09): «Открыть Postman», «Новое письмо», разделитель, «Выйти» — на обоих языках.
    #[test]
    fn menu_bar_menu_items() {
        let menu = menu_bar_menu();
        let signals: Vec<Option<Signal>> = menu.iter().map(|e| e.as_ref().map(|(s, _)| s.clone())).collect();
        assert_eq!(signals, [Some(Signal::Open), Some(Signal::Compose), None, Some(Signal::Quit)]);
        let texts = crate::parts::texts();
        for (lang, want) in [("ru", ["Открыть Postman", "Новое письмо", "Выйти"]), ("en", ["Open Postman", "New mail", "Quit"])]
        {
            let (i18n, _) = pets_i18n::I18n::new(&texts, lang);
            let labels: Vec<String> = menu.iter().flatten().map(|(_, key)| i18n.get(key)).collect();
            assert_eq!(labels, want, "{lang}");
        }
    }
}
