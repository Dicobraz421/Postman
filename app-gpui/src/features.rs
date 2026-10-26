//! «Нет у сервера — нет на экране» (S7-03, `docs/specs/S7.md`): что умеет сервер учётной записи
//! (`postman_core::model::Features`, приходит `sync::Event::Features`) решает, что видно. Одно правило на все функции:
//! - кнопки рейла, разделы настроек и кнопки разделов, которым нужна отсутствующая функция, не показываются;
//! - в палитре команд таких команд нет;
//! - их клавиши показывают тост «Нет у сервера этой учётной записи» ([`Unavailable`]), а не молчат.
//!
//! Пока сервер не ответил (и в демо) — всё есть: на время входа разделы не прячутся.

use gpui_kit::*;
use postman_core::model::{Feature, Features};

gpui_kit::actions!(features, [Unavailable]);

struct Current(Features);

impl Global for Current {}

/// Что умеет сервер сейчас.
pub(crate) fn get(cx: &App) -> Features {
    cx.try_global::<Current>().map(|c| c.0).unwrap_or_default()
}

/// Сервер сказал, что умеет: клавиши — заново (отсутствующее — тостом), окна — перерисовать. То же — ничего.
pub(crate) fn set(features: Features, cx: &mut App) {
    if get(cx) == features {
        return;
    }
    cx.set_global(Current(features));
    crate::commands::bind(cx, &crate::prefs::get(cx));
    cx.refresh_windows();
}

/// Что нужно команде от сервера — хотя бы одна из функций; пусто — ничего.
pub(crate) fn needs(id: &str) -> &'static [Feature] {
    const CALENDAR: &[Feature] = &[Feature::Calendar];
    match id {
        "view.calendar" | "settings.calendar" | "contacts.meeting" => CALENDAR,
        _ if id.starts_with("calendar.") || id.starts_with("meeting.") => CALENDAR,
        "view.tasks" => &[Feature::Tasks],
        _ if id.starts_with("task.") => &[Feature::Tasks],
        "view.contacts" => &[Feature::Contacts, Feature::Directory],
        "contacts.new" | "contacts.edit" | "contacts.delete" => &[Feature::Contacts],
        "settings.rules" => &[Feature::Rules],
        "settings.vacation" => &[Feature::Autoreply],
        _ => &[],
    }
}

/// Доступна ли команда при таких возможностях сервера.
pub(crate) fn available(id: &str, features: Features) -> bool {
    let need = needs(id);
    need.is_empty() || need.iter().any(|&f| features.has(f))
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "features", scenes: scenes::SCENES, reset: Some(scenes::reset), ..crate::part::Part::NONE };

/// Сцены `--screens` «Нет у сервера — нет на экране» (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::model::Features;

    use crate::screens::{self, Scene};
    use crate::settings_tab::Section as SettingsSection;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // «Нет у сервера — нет на экране» (S7-03): сервер только с почтой — рейл и разделы настроек без лишнего.
        Scene::new("features-mail-only", mail_only, mail_only_ready),
        // То же — «Настройки»: без «Календаря», «Правил» и «Автоответа».
        Scene::new("features-mail-only-settings", settings, settings_ready),
    ];

    /// Сервер умеет всё — кроме своих сцен (S7-03).
    pub(crate) fn reset(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        if !s.name.starts_with("features-") && super::get(cx) != Features::ALL {
            m.set_features(Features::ALL, cx);
        }
    }

    fn mail_only(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Открыт календарь — сервер без него возвращает в «Почту».
        m.close_settings(window, cx);
        m.set_section(Section::Calendar, cx);
        m.set_features(Features::NONE, cx);
        Ok(())
    }

    fn mail_only_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        !m.tabs.is_active(crate::tabs::TabKind::Settings) && m.section == Section::Mail && super::get(cx) == Features::NONE
    }

    fn settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_features(Features::NONE, cx);
        m.open_settings(Some(SettingsSection::General), window, cx);
        Ok(())
    }

    fn settings_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::settings_on(m, SettingsSection::General, cx) && super::get(cx) == Features::NONE
    }
}

#[cfg(test)]
mod tests {
    use super::{available, needs};
        use postman_core::model::{Feature, Features};

    #[test]
    fn everything_is_available_while_the_server_has_everything() {
        assert!(crate::commands::all().iter().all(|c| available(c.id, Features::ALL)));
    }

    #[test]
    fn mail_stays_without_anything_else() {
        let mail = Features::NONE;
        for id in ["mail.reply", "mail.new", "mail.due", "search.focus", "settings.open", "view.mail", "invite.accept"] {
            assert!(available(id, mail), "{id}");
        }
        for id in ["view.calendar", "calendar.new_meeting", "meeting.send", "view.tasks", "task.new", "view.contacts",
            "contacts.new", "contacts.meeting", "settings.rules", "settings.vacation", "settings.calendar"]
        {
            assert!(!available(id, mail), "{id}");
        }
    }

    #[test]
    fn contacts_section_stays_with_the_directory_alone() {
        let directory = Features::NONE.with(Feature::Directory, true);
        assert!(available("view.contacts", directory));
        assert!(!available("contacts.new", directory));
    }

    #[test]
    fn every_feature_rule_names_a_real_command() {
        // Правило по префиксу (`calendar.`, `meeting.`, `task.`) не должно задевать команды других разделов.
        for c in crate::commands::all() {
            let need = needs(c.id);
            let section = c.id.split('.').next().unwrap_or_default();
            if ["mail", "thread", "attach", "filters", "tab", "search", "palette", "compose", "invite"].contains(&section) {
                assert!(need.is_empty(), "{}", c.id);
            }
        }
    }
}
