//! «Нет у сервера — нет на экране» (S7-03, `docs/specs/S7.md`): что умеет сервер учётной записи
//! (`postman_core::model::Features`, приходит `sync::Event::Features`) решает, что видно. Одно правило на все функции:
//! - кнопки рейла, разделы настроек и кнопки разделов, которым нужна отсутствующая функция, не показываются;
//! - в палитре команд таких команд нет;
//! - их клавиши показывают тост «Нет у сервера этой учётной записи» ([`Unavailable`]), а не молчат.
//!
//! Пока сервер не ответил — всё есть: на время входа разделы не прячутся. В демо — функции учётной записи открытой
//! папки ([`of_demo_folder`], S7-09б): у «стандартов» нет справочника, правил и автоответа, у Exchange — задач,
//! контактов, правил, автоответа и ключевых слов (категорий, сроков, «Ждём»).

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

/// Функции учётной записи демо (`postman_demo::Has` — те же, что `Feature`, один к одному).
pub(crate) fn of_demo(has: postman_demo::Has) -> Features {
    [
        (Feature::Calendar, has.calendar),
        (Feature::Tasks, has.tasks),
        (Feature::Contacts, has.contacts),
        (Feature::Directory, has.directory),
        (Feature::FreeBusy, has.free_busy),
        (Feature::Rules, has.rules),
        (Feature::Autoreply, has.autoreply),
        (Feature::Quota, has.quota),
        (Feature::Keywords, has.keywords),
    ]
    .into_iter()
    .fold(Features::NONE, |f, (feature, on)| f.with(feature, on))
}

/// Демо: функции учётной записи, чья папка `folder` (текущая — та, чья папка выбрана; S7, «Несколько учётных
/// записей»). Папка общего ящика — его учётки; неизвестная — всё есть.
pub(crate) fn of_demo_folder(folder: &str) -> Features {
    let profile = postman_cache::profile_of(postman_cache::account_of(folder));
    postman_demo::account(profile).map_or(Features::ALL, |a| of_demo(a.has))
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
        // Категории, сроки, «Напомнить», «Ждать ответа», «Исчезнет» — ключевые слова письма на сервере.
        "mail.categorize" | "mail.due" | "mail.remind" | "mail.wait" | "mail.expire" => &[Feature::Keywords],
        _ => &[],
    }
}

/// Доступна ли команда при таких возможностях сервера.
pub(crate) fn available(id: &str, features: Features) -> bool {
    let need = needs(id);
    need.is_empty() || need.iter().any(|&f| features.has(f))
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "features", scenes: scenes::SCENES, reset: Some(scenes::reset), ..crate::part::Part::NONE };

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
        // Демо, выбрана папка учётки Exchange (S7-09б): функции — её сервера; в рейле нет «Задач», в закладках быстрых
        // фильтров — «Срок», «Ждём», «Истекают» (ключевых слов у Exchange нет).
        Scene::new("features-exchange", exchange, exchange_ready),
        // То же, письмо открыто: в шапке нет кнопок сроков.
        Scene::new("features-exchange-letter", exchange_letter, exchange_letter_ready).continues(),
    ];

    /// Входящие учётки Exchange демо.
    fn exchange_inbox() -> String {
        postman_cache::scoped(postman_demo::EXCHANGE.profile, "inbox")
    }

    fn exchange(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.open_mailbox(exchange_inbox(), cx);
        Ok(())
    }

    fn exchange_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::rows_loaded(m.list.read(cx))
            && m.folders.read(cx).current.as_deref() == Some(exchange_inbox().as_str())
            && super::get(cx) == super::of_demo(postman_demo::EXCHANGE.has)
    }

    fn exchange_letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.list.update(cx, |l, cx| l.select(0, cx));
        Ok(())
    }

    fn exchange_letter_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        let first = list.row(0).map(|r| r.id.as_str());
        list.selected == Some(0)
            && first.is_some()
            && screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && Some(r.row.id.as_str()) == first)
            && super::get(cx) == super::of_demo(postman_demo::EXCHANGE.has)
    }

    /// Сервер умеет всё — кроме своих сцен (S7-03).
    pub(crate) fn reset(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        if !s.name.starts_with("features-") && super::get(cx) != Features::ALL {
            m.set_features(Features::ALL, cx);
        }
    }

    /// Сервер только с почтой — JMAP без календаря, контактов, Sieve: ключевые слова у почты JMAP есть.
    fn mail() -> Features {
        Features::NONE.with(postman_core::model::Feature::Keywords, true)
    }

    fn mail_only(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Открыт календарь — сервер без него возвращает в «Почту».
        m.close_settings(window, cx);
        m.set_section(Section::Calendar, cx);
        m.set_features(mail(), cx);
        Ok(())
    }

    fn mail_only_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        !m.tabs.is_active(crate::tabs::TabKind::Settings) && m.section == Section::Mail && super::get(cx) == mail()
    }

    fn settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_features(mail(), cx);
        m.open_settings(Some(SettingsSection::General), window, cx);
        Ok(())
    }

    fn settings_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::settings_on(m, SettingsSection::General, cx) && super::get(cx) == mail()
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
        for id in ["mail.reply", "mail.new", "search.focus", "settings.open", "view.mail", "invite.accept"] {
            assert!(available(id, mail), "{id}");
        }
        for id in [
            "view.calendar",
            "calendar.new_meeting",
            "meeting.send",
            "view.tasks",
            "task.new",
            "view.contacts",
            "contacts.new",
            "contacts.meeting",
            "settings.rules",
            "settings.vacation",
            "settings.calendar",
            "mail.due",
            "mail.wait",
            "mail.categorize",
        ] {
            assert!(!available(id, mail), "{id}");
        }
        assert!(available("mail.due", mail.with(Feature::Keywords, true)));
    }

    #[test]
    fn contacts_section_stays_with_the_directory_alone() {
        let directory = Features::NONE.with(Feature::Directory, true);
        assert!(available("view.contacts", directory));
        assert!(!available("contacts.new", directory));
    }

    #[test]
    fn demo_accounts_have_their_server_features() {
        use super::{of_demo, of_demo_folder};
        assert_eq!(of_demo(postman_demo::MAIN.has), Features::ALL);
        let standards = of_demo(postman_demo::STANDARDS.has);
        assert!(available("view.tasks", standards) && available("contacts.new", standards));
        assert!(!standards.has(Feature::Directory));
        assert!(!available("settings.rules", standards) && !available("settings.vacation", standards));
        let exchange = of_demo(postman_demo::EXCHANGE.has);
        assert!(available("view.calendar", exchange) && available("view.contacts", exchange));
        for id in ["view.tasks", "task.new", "contacts.new", "settings.rules", "settings.vacation", "mail.categorize", "mail.due", "mail.wait"] {
            assert!(!available(id, exchange), "{id}");
        }
        // Папка — учётки, чей префикс у её ключа; основная — без префикса.
        assert_eq!(of_demo_folder("inbox"), Features::ALL);
        for a in &postman_demo::ACCOUNTS[1..] {
            let folder = postman_cache::scoped(a.profile, "inbox");
            assert_eq!(of_demo_folder(&folder), of_demo(a.has), "{folder}");
        }
    }

    #[test]
    fn every_feature_rule_names_a_real_command() {
        // Правило по префиксу (`calendar.`, `meeting.`, `task.`) не должно задевать команды других разделов; у почты —
        // только ключевые слова.
        for c in crate::commands::all() {
            let need: Vec<Feature> = needs(c.id).iter().copied().filter(|&f| f != Feature::Keywords).collect();
            let section = c.id.split('.').next().unwrap_or_default();
            if ["mail", "thread", "attach", "filters", "tab", "search", "palette", "compose", "invite"].contains(&section) {
                assert!(need.is_empty(), "{}", c.id);
            }
        }
    }
}
