//! Linux (LIN-03): тёмная/светлая тема и акцент среды — portal `org.freedesktop.appearance` (`color-scheme`,
//! `accent-color`; GNOME 47+, KDE 6). Тему окна gpui берёт из того же portal и меняет на лету (`roles::observe_window`),
//! но начальное значение и акцент `pets_ui` на Linux не читает — их передаёт этот модуль в `Themes`. Portal нет — тема
//! светлая, акцент Fluent (как раньше), INFO в журнал.

use ashpd::desktop::settings::{ColorScheme, Settings};
use futures::StreamExt;
use gpui_kit::App;
use pets_ui::theme::{AccentPalette, Color, OsLook, Themes};

/// Вид среды: тёмная и акцент; `None` у акцента — среда его не задала.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    dark: bool,
    accent: Option<Color>,
}

impl Look {
    fn os(self) -> OsLook {
        OsLook { dark: self.dark, accent: self.accent.map(AccentPalette::from_base).unwrap_or_default(), ..OsLook::default() }
    }
}

/// Цвет portal (доли 0…1 в sRGB); вне диапазона — «не задан» (спецификация portal).
fn color((r, g, b): (f64, f64, f64)) -> Option<Color> {
    let byte = |v: f64| (0.0..=1.0).contains(&v).then(|| (v * 255.0).round() as u8);
    Some(Color { r: byte(r)?, g: byte(g)?, b: byte(b)?, a: 255 })
}

/// Прочитать вид среды и следить за ним в своём потоке; каждое изменение — в `Themes` (поток интерфейса).
pub fn watch(cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Look>();
    let spawned = std::thread::Builder::new().name("appearance".into()).spawn(move || {
        futures::executor::block_on(async move {
            let settings = match Settings::new().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::info!("appearance: settings portal not available, light theme and default accent: {e}");
                    return;
                }
            };
            let dark = |s: ColorScheme| s == ColorScheme::PreferDark;
            let mut look = Look {
                dark: settings.color_scheme().await.is_ok_and(dark),
                accent: settings.accent_color().await.ok().and_then(|c| color((c.red(), c.green(), c.blue()))),
            };
            tracing::debug!(?look, "appearance: desktop look");
            if tx.unbounded_send(look).is_err() {
                return;
            }
            let (Ok(schemes), Ok(accents)) = (settings.receive_color_scheme_changed().await, settings.receive_accent_color_changed().await) else {
                tracing::info!("appearance: desktop look changes are not watched");
                return;
            };
            let mut changes = futures::stream::select(
                schemes.map(move |s| Change::Dark(dark(s))),
                accents.map(|c| Change::Accent(color((c.red(), c.green(), c.blue())))),
            );
            while let Some(change) = changes.next().await {
                match change {
                    Change::Dark(d) => look.dark = d,
                    Change::Accent(a) => look.accent = a,
                }
                tracing::info!(?look, "appearance: desktop look changed");
                if tx.unbounded_send(look).is_err() {
                    return;
                }
            }
        });
    });
    if let Err(e) = spawned {
        tracing::warn!("appearance: watcher thread not started: {e}");
        return;
    }
    cx.spawn(async move |cx| {
        while let Some(look) = rx.next().await {
            cx.update(|cx| Themes::update(cx, |s| s.set_os(look.os())));
        }
    })
    .detach();
}

enum Change {
    Dark(bool),
    Accent(Option<Color>),
}

#[cfg(test)]
mod tests {
    /// Доли portal → байты; вне 0…1 — акцент не задан.
    #[test]
    fn portal_color() {
        assert_eq!(super::color((0.0, 0.5, 1.0)).map(|c| (c.r, c.g, c.b)), Some((0, 128, 255)));
        assert_eq!(super::color((-1.0, 0.5, 1.0)), None);
        assert_eq!(super::color((0.2, 1.5, 0.0)), None);
    }
}
