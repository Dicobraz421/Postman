//! Сценарии с вводом (`Scene::input`) для настроек, календаря, встреч, контактов: шаги `docs/MANUAL-TESTS.md` и
//! `docs/manual-tests/`. Имена `cal-<раздел>-<что>`, запуск — `cal_drills` в `app-gpui/tests/manual.rs`.
//!
//! Сцены `cal-…-kept` — второй запуск с тем же каталогом данных: «после перезапуска». Обоим запускам нужен
//! `POSTMAN_SCREENS_PREFS=1`, иначе снимки не читают и не пишут файл настроек; второй — без `--lang` и `--theme`: они
//! сильнее файла.

use std::cell::{Cell, RefCell};

use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{FLUENT_DARK, Themes};
use postman_core::prefs::{Calendar, CalendarView as V, FreeColor, Pane, Prefs};

use crate::compose::Field;
use crate::meeting::MeetingView;
use crate::screens::{self, Held, Input, Scene, compose_tab, rows_loaded, settings_on, spot_bounds, spot_center};
use crate::settings_tab::{Section as S, SettingsView};
use crate::tabs::TabKind;
use crate::{Mailer, Section};

pub(crate) const PART: crate::part::Part = crate::part::Part { name: "drill-cal", scenes: SCENES, ..crate::part::Part::NONE };

const SCENES: &[Scene] = &[
    // Раздел 24, шаг 1: шестерёнка, Ctrl+, и «…» открывают одну вкладку; Ctrl+W, «×», Esc закрывают.
    Scene::new("cal-24-open", start, settings_closed).input(OPEN).on_request(),
    // Раздел 24, шаг 2: поиск разделов, Enter открывает первый найденный.
    Scene::new("cal-24-search", settings, on_keys).input(SEARCH).on_request(),
    // Раздел 24, шаг 4: цвет и клавиша заняты, новая категория, полоса в списке писем.
    Scene::new("cal-24-cats", categories, row_has_new_category).input(CATS).on_request(),
    // Раздел 24, шаг 5: удалить, вернуть, «Снять с писем» со строкой подтверждения.
    Scene::new("cal-24-purge", categories, purge_asked_and_closed).input(PURGE).on_request(),
    // Раздел 24, шаг 6: своя клавиша, конфликт, сброс, Esc во время записи.
    Scene::new("cal-24-keys", keyboard, keys_clean).input(KEYS).on_request(),
    // Раздел 24, шаг 8: две подписи, умолчания для новых и ответов, меню «Подпись» письма.
    Scene::new("cal-24-sign", signatures, reply_signed).input(SIGN).on_request(),
    // Раздел 24, шаг 10 (демо): «Добавить аккаунт» на месте, правила и автоответы — разделами.
    Scene::new("cal-24-accounts", accounts, on_vacation).input(ACCOUNTS).on_request(),
    // Раздел 42, шаг 3: цвета прежней версии — цвета таблицы, после правки — новым именем.
    Scene::new("cal-42-old-colors", old_colors, old_color_renamed).input(OLD_COLORS).on_request(),
    // Раздел 36, шаг 2: рабочие дни и часы. Затенение недели — на снимке.
    Scene::new("cal-36-hours", calendar_settings, week_shown).input(HOURS).on_request(),
    // Раздел 36, шаг 4 и S5-21, шаг 4: пояс, «Как в системе», первый день, вид, шаг, второй пояс.
    Scene::new("cal-36-zone", calendar_settings, zone_saved).input(ZONE).on_request(),
    // Раздел 24, шаг 7: панель «Снизу» и «Скрыта», письмо вместо списка, Esc — назад.
    Scene::new("cal-24-reading", reading, pane_saved).input(READING).on_request(),
    // Раздел 24, шаг 3: тема, плотность и язык сразу и в файле настроек.
    Scene::new("cal-24-look", look, look_saved).input(LOOK).on_request(),
    // Второй запуск: разделы 24 (шаги 3, 7), 36 (шаг 4) и S5-21 (шаг 4), затем «По умолчанию» — всё к макету.
    Scene::new("cal-36-kept", calendar_settings, calendar_default).input(KEPT).on_request(),
    // Раздел 38, шаг 2: ←/→ по общим окнам, край дня, Enter; в поле «Тема» — курсор текста.
    Scene::new("cal-38-arrows", meeting, title_typed).input(ARROWS).on_request(),
    // Раздел 38, шаг 3: длительность 2 ч — окна короче пропали, ←/→ по оставшимся.
    Scene::new("cal-38-long", meeting, cursor_on_free).input(LONG).on_request(),
    // Раздел 38, шаг 6: рамка и ручки мышью, край дня, Shift+→.
    Scene::new("cal-38-drag", meeting, widened).input(DRAG).on_request(),
    // Раздел 38, шаг 7: PgDn, PgUp, Home вне полей; в поле — не день.
    Scene::new("cal-38-days", meeting, date_kept).input(DAYS).on_request(),
    // Раздел 38, шаг 8: Ctrl+D и корзина закрывают вкладку.
    Scene::new("cal-38-discard", start, no_meeting).input(DISCARD).on_request(),
    // S7-07б, шаг 7: «Изменить» — вкладка встречи с правкой, отмена — тост «нужен сервер».
    Scene::new("cal-s707-own", own_meeting, needs_server).input(OWN).on_request(),
    // Разделы 26 (шаг 6) и 38 (шаг 4): вкладка встречи, название, чипы, подсказки, Esc.
    Scene::new("cal-26-meeting", start, no_meeting).input(MEETING).on_request(),
    // Раздел 26, шаг 12: Ctrl+Shift+C, F2, Delete → «Да».
    Scene::new("cal-26-contact", start, drill_contact_gone).input(CONTACT).on_request(),
    // Раздел 13, шаг 3: «Написать письмо» — письмо с адресом.
    Scene::new("cal-13-write", start, letter_to_contact).input(WRITE).on_request(),
    // Раздел 13, шаг 5: подсказки адресов в письме (во встрече — `cal-26-meeting`).
    Scene::new("cal-13-suggest", start, letter_suggests).input(SUGGEST).on_request(),
    // Раздел 43, шаг 2: поиск кириллицей, в английской раскладке и латиницей, группами.
    Scene::new("cal-43-search", start, same_people).input(PEOPLE).on_request(),
    // S5-21, шаги 1–3: клавиши 1–4, PgUp/PgDn, Home, «+N ещё» — к дню.
    Scene::new("cal-s521-views", start, month_more_opened).input(VIEWS).on_request(),
    // S5-21, шаг 7: Ctrl+E, ↑/↓, Enter, Esc.
    Scene::new("cal-s521-search", start, search_cleared).input(CAL_SEARCH).on_request(),
    // S5-22, шаг 2: флажок календаря коллеги снимается и возвращается, выбор — в настройках.
    Scene::new("cal-s522-check", start, colleagues_shown).input(COLLEAGUES).on_request(),
    // S5-22, шаг 3: «Цвета по владельцу» и «Показывать имя владельца» выключены, неделя перерисована.
    Scene::new("cal-s522-owner", calendar_bottom, owner_off_in_week).input(OWNER).on_request(),
    // S8-44, шаг 1: ссылка `http://` отвергнута, поле открыто.
    Scene::new("cal-s844-http", start, subscription_refused).input(SUBSCRIPTION).on_request(),
    // S8-42, шаги 1, 2: «Указать ссылку» у внешнего участника, `http://` отвергнут, строка не закрылась.
    Scene::new("cal-s842-link", meeting, link_refused).input(LINK).on_request(),
    // #552: ответ сервера о занятости при открытой встрече — без краша (вкладка не читает `Mailer` в его обновлении).
    Scene::new("cal-552-server-busy", meeting, server_busy_taken).input(SERVER_BUSY).on_request(),
    // Раздел 41, шаг 2: узкое окно, кнопки карточки приглашения не вылезают за панель.
    Scene::new("cal-41-narrow", narrow, invite_fits).input(INVITE).wait(new_invite).on_request(),
    // Раздел 41, шаг 3: схема «Одиночные клавиши», A в списке — ответ; демо — тост «нужен сервер».
    Scene::new("cal-41-answer", wide, answered_again).input(ANSWER).wait(new_invite).on_request(),
    // Решение владельца п. 11: поиск, Enter по встрече в субботу при «Раб. неделе»: вид «Неделя», событие видно и выбрано.
    Scene::new("cal-s521-weekend", start, weekend_shown).input(WEEKEND_SEARCH).on_request(),
    // Решение владельца п. 9: подробности события и стопка тостов: «Написать участникам» свободна.
    Scene::new("cal-d9-event", start, write_free).input(EVENT_TOASTS).on_request(),
    // Решение владельца п. 9: поле поиска «Клавиатура» берёт фокус с первого щелчка при стопке тостов.
    Scene::new("cal-d9-keys", keyboard, key_typed).input(KEYS_UNDER_TOASTS).on_request(),
    // T3-08: конец серии в подробностях события — «По будням, до 30.12.2027» (год не нынешний), затем «…, 5 раз».
    Scene::new("cal-t308-until", series_end, series_shown).on_request(),
    Scene::new("cal-t308-count", series_end, series_shown).continues().on_request(),
    // T3-05 (#552, п. 2): календари рядом. Сцены снимают флажки всех календарей, кроме Ивановой, — идут своим запуском с
    // `POSTMAN_SCREENS_PREFS=1`, по порядку. Раб. неделя наложением (как было, по умолчанию).
    Scene::new("cal-t305-overlay", one_colleague, overlay_shown).input(SIDE_OVERLAY).on_request(),
    // «Рядом» в командной строке: мой календарь и Ивановой — двумя блоками со своими шапками, наложения — в своём блоке.
    Scene::new("cal-t305-side", in_cal, side_shown).input(SIDE_ON).on_request(),
    // Тот же режим в виде «День».
    Scene::new("cal-t305-day", in_cal, side_day).input(SIDE_DAY).on_request(),
    // «‹» в шапке блока Ивановой — снова наложением, раб. неделя.
    Scene::new("cal-t305-back", in_cal, overlay_back).input(SIDE_BACK).on_request(),
    // Палитра: «рядом», Enter — снова рядом, режим записан в файл настроек.
    Scene::new("cal-t305-palette", in_cal, side_saved).input(SIDE_PALETTE).on_request(),
    // Второй запуск с тем же каталогом данных: календари рядом сразу, из настроек.
    Scene::new("cal-t305-kept", start, side_kept).input(SIDE_KEPT).on_request(),
];

/// Планёрка демо (по будням) и правило с концом серии сцены `cal-t308-…`.
const SERIES: &str = "Планёрка отдела"; // i18n-ok: тема в демо-данных

fn series_rule(scene: &Scene) -> &'static str {
    if scene.name == "cal-t308-count" { "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;COUNT=5" } else { "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;UNTIL=20271230T235959" }
}

/// Планёрка с концом серии — в подробностях.
fn series_end(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    let rule = series_rule(s);
    m.cal.update(cx, |c, cx| {
        c.set_view(V::WorkWeek, cx);
        c.go_to_demo_week(cx);
        for e in c.events.iter_mut().filter(|e| e.title == SERIES) {
            e.recurrence = Some(rule.into());
        }
        c.select_titled(SERIES, cx);
    });
    m.set_section(Section::Calendar, cx);
    Ok(())
}

fn series_shown(s: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    m.section == Section::Calendar && c.selected() && c.events.iter().any(|e| e.title == SERIES && e.recurrence.as_deref() == Some(series_rule(s)))
}

const OPEN: &[Input] = &[
    Input::Until(mail_ready),
    Input::Press("nav-settings"),
    Input::Until(settings_open),
    Input::Keys(&["ctrl-w"]),
    Input::Until(settings_closed),
    Input::Keys(&["ctrl-,"]),
    Input::Until(settings_open),
    Input::Keys(&["ctrl-,"]),
    Input::Until(one_settings_tab),
    Input::Click(tab_close, Held::No),
    Input::Until(settings_closed),
    Input::Press("bar-more"),
    Input::Press("more-settings"),
    Input::Until(settings_open),
    Input::Keys(&["escape"]),
];

const SEARCH: &[Input] = &[
    Input::Press("settings-search"),
    Input::Text("пароль"), // i18n-ok: ввод сценария
    Input::Until(first_is_accounts),
    Input::Keys(&["enter"]),
    Input::Until(on_accounts),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text("клавиши"), // i18n-ok: ввод сценария
    Input::Until(first_is_keys),
    Input::Keys(&["enter"]),
];

const CATS: &[Input] = &[
    Input::Press("settings-cat-add"),
    Input::Until(editing_category),
    Input::Press("settings-cat-color-0"),
    Input::Until(color_refused),
    Input::Press("settings-cat-key-1"),
    Input::Until(key_refused),
    Input::Press("settings-cat-cancel"),
    Input::Until(not_editing_category),
    Input::Press("settings-cat-delete-4"),
    Input::Until(four_categories),
    Input::Press("settings-cat-add"),
    Input::Until(editing_category),
    Input::Text(NEW_CATEGORY),
    Input::Press("settings-cat-save"),
    Input::Until(category_added),
    Input::Keys(&["ctrl-w"]),
    Input::Until(settings_closed),
    Input::Until(mail_ready),
    Input::Click(row0, Held::No),
    Input::Until(row0_selected),
    // Снять категории письма: меток в строке две, полоса — цвет первой.
    Input::Keys(&["1", "2", "5"]),
];

const PURGE: &[Input] = &[
    Input::Press("settings-cat-delete-0"),
    Input::Until(four_categories),
    Input::Press("settings-cat-restore-0"),
    Input::Until(five_categories),
    Input::Press("settings-cat-delete-0"),
    Input::Until(four_categories),
    Input::Press("settings-cat-purge-0"),
    Input::Until(purge_line),
    Input::Press("settings-cat-purge-yes-0"),
];

const KEYS: &[Input] = &[
    Input::Press("settings-keys-search"),
    Input::Text("флаг"), // i18n-ok: ввод сценария
    Input::Until(one_key_row),
    Input::Press("settings-keys-edit-0"),
    Input::Until(recording_flag),
    Input::Keys(&["ctrl-shift-g"]),
    Input::Until(flag_on_new_key),
    Input::Keys(&["ctrl-1"]),
    Input::Until(mail_ready),
    Input::Click(row0, Held::No),
    Input::Until(row0_selected),
    Input::Until(remember_flag),
    Input::Keys(&["ctrl-shift-g"]),
    Input::Until(flag_changed),
    Input::Keys(&["ctrl-,"]),
    Input::Until(on_keys),
    Input::Press("settings-keys-edit-0"),
    Input::Until(recording_flag),
    Input::Keys(&["ctrl-r"]),
    Input::Until(clash_shown),
    Input::Press("settings-keys-both-0"),
    Input::Until(conflict_listed),
    Input::Press("settings-keys-reset-0"),
    Input::Until(no_own_keys),
    Input::Press("settings-keys-edit-0"),
    Input::Until(recording_flag),
    Input::Keys(&["escape"]),
    Input::Until(recording_stopped),
    Input::Press("settings-keys-edit-0"),
    Input::Until(recording_flag),
    Input::Keys(&["ctrl-r"]),
    Input::Until(clash_shown),
    Input::Press("settings-keys-replace-0"),
    Input::Until(flag_replaced_reply),
    Input::Press("settings-keys-reset-all"),
];

const SIGN: &[Input] = &[
    Input::Press("settings-sign-add"),
    Input::Until(editing_signature),
    Input::Text("Первая"), // i18n-ok: ввод сценария
    Input::Press("settings-sign-text"),
    Input::Text(SIGN_NEW),
    Input::Press("settings-sign-save"),
    Input::Until(one_signature),
    Input::Press("settings-sign-add"),
    Input::Until(editing_signature),
    Input::Text("Вторая"), // i18n-ok: ввод сценария
    Input::Press("settings-sign-text"),
    Input::Text(SIGN_REPLY),
    Input::Press("settings-sign-save"),
    Input::Until(two_signatures),
    Input::Press("settings-sign-default-1"),
    Input::Press("settings-sign-default-66"),
    Input::Until(defaults_set),
    Input::Keys(&["ctrl-1"]),
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-n"]),
    Input::Until(letter_new_signed),
    Input::Press("tb-signature"),
    Input::Until(signature_menu_lists_both),
    Input::Keys(&["escape", "ctrl-w"]),
    Input::Until(no_letters),
    Input::Keys(&["ctrl-1"]),
    Input::Until(mail_ready),
    Input::Click(row0, Held::No),
    Input::Until(row0_selected),
    Input::Keys(&["ctrl-r"]),
];

const ACCOUNTS: &[Input] = &[
    Input::Press("settings-account-add"),
    Input::Until(add_account_shown),
    Input::Press("settings-nav-11"),
    Input::Until(on_rules),
    Input::Press("settings-nav-12"),
];

const OLD_COLORS: &[Input] = &[Input::Press("settings-cat-edit-0"), Input::Until(editing_category), Input::Press("settings-cat-save")];

const HOURS: &[Input] = &[
    Input::Press("settings-cal-day-0"),
    Input::Until(monday_off),
    Input::Press("settings-cal-select-0"),
    Input::Press("settings-cal-item-3"),
    Input::Until(start_ten),
    Input::Press("settings-cal-select-1"),
    Input::Press("settings-cal-item-101"),
    Input::Until(end_five),
    Input::Press("settings-cal-select-2"),
    Input::Until(zone_list_open),
    Input::Keys(&["escape"]),
    Input::Until(zone_list_closed),
    Input::Keys(&["ctrl-2"]),
    Input::Until(in_calendar),
    // «Раб. неделя» показывает только рабочие дни: затенённый понедельник — в виде «Неделя».
    Input::Keys(&["3"]),
];

const ZONE: &[Input] = &[
    Input::Press("settings-cal-select-2"),
    Input::Press(URAL_ITEM),
    Input::Until(zone_ural),
    Input::Keys(&["ctrl-2"]),
    Input::Until(calendar_in_ural),
    Input::Keys(&["ctrl-,"]),
    Input::Until(on_calendar_settings),
    Input::Press("settings-cal-select-2"),
    Input::Press("settings-cal-item-200"),
    Input::Until(zone_windows),
    Input::Press("settings-cal-select-2"),
    Input::Press(URAL_ITEM),
    Input::Until(zone_ural),
    Input::Press("settings-cal-first-1"),
    Input::Until(first_sunday),
    Input::Press("settings-cal-view-2"),
    Input::Until(view_chosen),
    Input::Press("settings-cal-step-15"),
    Input::Until(step_quarter),
    Input::Press("settings-cal-second"),
];

const READING: &[Input] = &[
    Input::Press("settings-pane-1"),
    Input::Until(pane_bottom),
    Input::Press("settings-pane-2"),
    Input::Until(pane_hidden),
    Input::Keys(&["ctrl-1"]),
    Input::Until(mail_ready),
    Input::Until(list_drawn),
    Input::Click(row0, Held::No),
    Input::Until(letter_instead_of_list),
    Input::Keys(&["escape"]),
    Input::Until(list_back),
];

const LOOK: &[Input] = &[
    Input::Press("settings-theme-2"),
    Input::Until(dark),
    Input::Press("settings-density-20"),
    Input::Until(compact),
    Input::Press("settings-nav-0"),
    Input::Until(on_general),
    Input::Press("settings-lang-1"),
    Input::Until(english),
];

const KEPT: &[Input] = &[Input::Until(all_kept), Input::Press("settings-cal-reset")];

const ARROWS: &[Input] = &[
    Input::Until(meeting_ready),
    Input::Click(grid_gap, Held::No),
    // Шаг идёт от выбранного времени: к левому краю дня, там — на месте.
    Input::Keys(&["left", "left", "left", "left", "left", "left", "left", "left"]),
    Input::Until(cursor_first),
    Input::Keys(&["left"]),
    Input::Until(cursor_first),
    Input::Keys(&["right"]),
    Input::Until(cursor_second),
    Input::Keys(&["left"]),
    Input::Until(cursor_first),
    Input::Keys(&["right", "right", "right", "right", "right", "right", "right", "right"]),
    Input::Until(cursor_last),
    Input::Keys(&["right"]),
    Input::Until(cursor_last),
    Input::Keys(&["enter"]),
    Input::Until(picked_cursor),
    Input::Press("meeting-title"),
    Input::Keys(&["end"]),
    Input::Text("ab"),
    Input::Keys(&["left"]),
    Input::Text("X"),
    Input::Keys(&["enter"]),
];

const LONG: &[Input] = &[
    Input::Until(meeting_ready),
    Input::Click(grid_gap, Held::No),
    Input::Press("duration-7200"),
    Input::Until(only_long_windows),
    Input::Keys(&["right"]),
];

const DRAG: &[Input] = &[
    Input::Until(meeting_ready),
    Input::Until(remember_picked),
    Input::Drag(picked_center, picked_later),
    Input::Until(moved_half_hour),
    Input::Until(remember_picked),
    Input::Drag(end_grip, end_later),
    Input::Until(end_half_hour_later),
    Input::Until(remember_picked),
    Input::Drag(start_grip, start_past_end),
    Input::Until(shortest_span),
    Input::Drag(picked_center, far_left),
    Input::Until(at_day_start),
    Input::Until(remember_picked),
    Input::Keys(&["shift-right"]),
];

const DAYS: &[Input] = &[
    Input::Until(meeting_ready),
    Input::Click(grid_gap, Held::No),
    Input::Until(remember_date),
    Input::Keys(&["pagedown"]),
    Input::Until(date_changed),
    Input::Keys(&["pageup"]),
    Input::Until(date_same),
    Input::Keys(&["home"]),
    Input::Until(date_today),
    Input::Until(remember_date),
    Input::Press("meeting-title"),
    Input::Keys(&["pagedown"]),
];

const DISCARD: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-shift-q"]),
    Input::Until(meeting_tab),
    Input::Keys(&["ctrl-d"]),
    Input::Until(no_meeting),
    Input::Keys(&["ctrl-shift-q"]),
    Input::Until(meeting_tab),
    Input::Press("discard"),
];

const MEETING: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-shift-q"]),
    Input::Until(meeting_tab),
    Input::Press("meeting-title"),
    Input::Text(NEW_CATEGORY),
    Input::Until(tab_named),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text("one@corp.example"),
    Input::Keys(&["enter"]),
    Input::Until(one_chip),
    Input::Text("two@corp.example;"),
    Input::Until(two_chips),
    Input::Until(book_ready),
    Input::Press("chip-x-0"),
    Input::Until(one_chip),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text("иван"), // i18n-ok: ввод сценария
    Input::Until(suggests_shown),
    Input::Keys(&["enter"]),
    Input::Until(two_chips),
    Input::Keys(&["escape"]),
];

const CONTACT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-3"]),
    Input::Until(contacts_open),
    Input::Keys(&["ctrl-shift-c"]),
    Input::Until(contact_form),
    Input::Text(NEW_CONTACT),
    Input::Press("contacts-emails"),
    Input::Text(CONTACT_MAIL),
    Input::Keys(&["enter"]),
    Input::Until(drill_contact_there),
    Input::Keys(&["f2"]),
    Input::Until(contact_form),
    Input::Keys(&["escape"]),
    Input::Until(no_contact_form),
    // Тост «Сохранено» лёг бы на строку подтверждения.
    Input::Until(no_toast),
    Input::Keys(&["delete"]),
    Input::Press("delete-yes"),
];

const WRITE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-3"]),
    Input::Until(contacts_open),
    Input::Until(contact_row_shown),
    Input::Click(first_contact, Held::No),
    Input::Until(contact_picked),
    Input::Press("contact-write"),
];

const SUGGEST: &[Input] =
    &[Input::Until(mail_ready), Input::Keys(&["ctrl-n"]), Input::Until(letter_open), Input::Until(book_ready), Input::Text("ива")]; // i18n-ok: ввод сценария

const PEOPLE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-3"]),
    Input::Until(contacts_open),
    Input::Keys(&["ctrl-e"]),
    Input::Text("ива"), // i18n-ok: ввод сценария
    Input::Until(found_cyrillic),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text("bdf"),
    Input::Until(found_layout),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text("ivanova"),
];

const VIEWS: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Until(in_calendar),
    Input::Keys(&["1"]),
    Input::Until(view_day),
    Input::Until(remember_day),
    Input::Keys(&["pagedown"]),
    Input::Until(day_after),
    Input::Keys(&["pageup"]),
    Input::Until(day_same),
    Input::Keys(&["pagedown", "pagedown", "home"]),
    Input::Until(day_today),
    Input::Keys(&["2"]),
    Input::Until(view_work_week),
    Input::Keys(&["3"]),
    Input::Until(view_week),
    Input::Keys(&["4"]),
    Input::Until(view_month),
    Input::Until(remember_day),
    Input::Keys(&["pagedown"]),
    Input::Until(other_month),
    Input::Keys(&["pageup"]),
    Input::Until(day_same),
    Input::Until(month_more_shown),
    Input::Click(month_more, Held::No),
];

const CAL_SEARCH: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Until(in_calendar),
    Input::Keys(&["2", "ctrl-e"]),
    Input::Text(ROOM),
    Input::Until(found_events),
    Input::Keys(&["down"]),
    Input::Until(second_result),
    Input::Keys(&["up"]),
    Input::Until(first_result),
    Input::Keys(&["enter"]),
    Input::Until(event_opened),
    Input::Keys(&["escape"]),
];

const TOASTS: Input = Input::Run(|_, m, _, cx| {
    crate::screens::toast_stack(m, cx);
    Ok(())
});

const EVENT_TOASTS: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Until(in_calendar),
    Input::Run(|_, m, _, cx| {
        m.cal.update(cx, |c, cx| {
            c.go_to_demo_week(cx);
            c.select_busiest(cx);
        });
        Ok(())
    }),
    Input::Until(|_, m, cx| m.cal.read(cx).selected()),
    TOASTS,
];

fn write_free(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.toasts.read(cx).on_screen() == 3 && crate::screens::apart("notice-stack", "write-all")
}

const KEYS_UNDER_TOASTS: &[Input] = &[
    TOASTS,
    Input::Until(|_, m, cx| m.toasts.read(cx).on_screen() == 3 && crate::screens::apart("notice-stack", "settings-keys-search")),
    Input::Press("settings-keys-search"),
    Input::Text("флаг"), // i18n-ok: ввод сценария
    Input::Until(one_key_row),
];

fn key_typed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.toasts.read(cx).on_screen() == 3 && one_key_row(&SCENES[0], m, cx)
}

const WEEKEND: &str = "Встреча в субботу"; // i18n-ok: ввод сценария
/// Суббота демо-недели, дней от понедельника.
const SATURDAY: i64 = 5;

const WEEKEND_SEARCH: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Until(in_calendar),
    Input::Run(|_, m, _, cx| {
        m.cal.update(cx, |c, cx| {
            c.go_to_demo_week(cx);
            c.add_demo_event(WEEKEND, SATURDAY, cx);
        });
        Ok(())
    }),
    Input::Keys(&["2", "ctrl-e"]),
    Input::Until(|_, m, cx| cal_view(m, cx) == V::WorkWeek),
    Input::Text(WEEKEND),
    Input::Until(|_, m, cx| m.cal.read(cx).results_ready()),
    Input::Keys(&["enter"]),
];

fn weekend_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let (view, days) = m.cal.read(cx).view_days();
    let saturday = days.iter().any(|d| d.weekday() == jiff::civil::Weekday::Saturday);
    view == V::Week && saturday && m.cal.read(cx).selected_title() == Some(WEEKEND)
}

const COLLEAGUES: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Until(in_calendar),
    Input::Press("cal-colleague-check-0"),
    Input::Until(colleague_hidden),
    Input::Press("cal-colleague-check-0"),
];

const SIDE_OVERLAY: &[Input] = &[Input::Until(mail_ready), Input::Run(only_ivanova), Input::Until(overlay_shown)];

const SIDE_ON: &[Input] = &[Input::Press("cal-mode-side"), Input::Until(side_shown)];

const SIDE_DAY: &[Input] = &[Input::Keys(&["1"]), Input::Until(side_day)];

const SIDE_BACK: &[Input] = &[Input::Keys(&["2"]), Input::Until(side_shown), Input::Press("cal-block-overlay-1")];

const SIDE_PALETTE: &[Input] = &[
    Input::Keys(&["ctrl-k"]),
    Input::Until(palette_open),
    Input::Text("рядом"), // i18n-ok: ввод сценария
    Input::Until(palette_one),
    Input::Keys(&["enter"]),
];

const SIDE_KEPT: &[Input] = &[Input::Until(mail_ready), Input::Keys(&["ctrl-2"]), Input::Until(in_calendar)];

const OWNER: &[Input] = &[
    Input::Press("settings-cal-owner-colors"),
    Input::Until(owner_colors_off),
    Input::Press("settings-cal-owner-names"),
    Input::Until(owner_names_off),
    Input::Keys(&["ctrl-2"]),
];

const SUBSCRIPTION: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Until(in_calendar),
    Input::Press("sub-open"),
    Input::Text(PLAIN_URL),
    Input::Keys(&["enter"]),
];

const LINK: &[Input] = &[Input::Until(meeting_ready), Input::Press(GUEST_LINK), Input::Text(PLAIN_URL), Input::Keys(&["enter"])];

const OWN: &[Input] = &[
    Input::Press("own-edit"),
    Input::Press("own-one"),
    Input::Until(editing_meeting),
    Input::Keys(&["ctrl-d"]),
    Input::Until(no_meeting),
    Input::Keys(&["ctrl-2"]),
    Input::Press("own-cancel"),
    Input::Press("own-one"),
];

const INVITE: &[Input] = &[Input::Until(mail_ready), Input::Click(invite_row, Held::No)];

const ANSWER: &[Input] = &[
    Input::Until(mail_ready),
    Input::Click(invite_row, Held::No),
    Input::Until(invite_card),
    Input::Press("invite-accept"),
    Input::Until(needs_server),
    Input::Until(forget_toasts),
    Input::Keys(&["a"]),
];

/// Имя новой категории и тема встречи.
const NEW_CATEGORY: &str = "Дрилл"; // i18n-ok: ввод сценария
const NEW_CONTACT: &str = "Дрилл Тестов"; // i18n-ok: ввод сценария
const CONTACT_MAIL: &str = "drill@corp.example";
const SIGN_NEW: &str = "С уважением"; // i18n-ok: ввод сценария
const SIGN_REPLY: &str = "Спасибо"; // i18n-ok: ввод сценария
/// Место нескольких встреч демо.
const ROOM: &str = "Переговорная 1"; // i18n-ok: ввод сценария
/// «(UTC+05:00) Екатеринбург»: пятый в списке «Пояс» (`Menu::Zone`), первый — «Как в системе».
const URAL_ITEM: &str = "settings-cal-item-204";
const URAL: &str = "Asia/Yekaterinburg";
/// Ссылка без TLS: календари и занятость её не принимают.
const PLAIN_URL: &str = "http://calendar.example.test/a.ics";
/// «Указать ссылку» у внешнего участника демо.
const GUEST_LINK: &str = "link-guest@partner.example";
/// Почта коллеги демо: «Иванова Мария».
const IVANOVA: &str = "m.ivanova@corp.example";

thread_local! {
    /// Запомненное значение между шагами: выбранное время, флаг, день.
    static MARK: Cell<(i64, i64)> = const { Cell::new((0, 0)) };
    static MARK_TEXT: RefCell<String> = const { RefCell::new(String::new()) };
}

// --- начало сцен ---

/// Вкладки прошлой сцены закрыты, клавиши — экрану почты: фокус в скрытой вкладке глотал бы команды.
fn start(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.close_all_meetings(window, cx);
    window.focus(&m.focus, cx);
    Ok(())
}

/// Вкладка с чистого листа: поиск и правки прошлой сцены с ней закрыты.
fn fresh(section: S, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) {
    m.close_settings(window, cx);
    m.open_settings(Some(section), window, cx);
}

fn settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::General, m, window, cx);
    Ok(())
}

fn categories(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Categories, m, window, cx);
    Ok(())
}

fn keyboard(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Keys, m, window, cx);
    Ok(())
}

fn signatures(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Signatures, m, window, cx);
    Ok(())
}

fn accounts(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Accounts, m, window, cx);
    Ok(())
}

fn calendar_settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Calendar, m, window, cx);
    Ok(())
}

fn reading(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Reading, m, window, cx);
    // Строка «Панель чтения» — ниже видов беседы и папок.
    if let Some(v) = m.settings.view.clone() {
        v.update(cx, |v, cx| {
            v.scroll.set_offset(point(px(0.), px(READING_SCROLL)));
            cx.notify();
        });
    }
    Ok(())
}

fn look(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Look, m, window, cx);
    Ok(())
}

/// Ящик прежней версии: цвет Fluent у категории по умолчанию и у своей.
fn old_colors(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Categories, m, window, cx);
    let Some(v) = m.settings.view.clone() else { return Err("no settings tab".into()) };
    v.update(cx, |v, cx| {
        let mut cats = v.cats.clone();
        if let Some(c) = cats.list.iter_mut().find(|c| !c.hidden) {
            c.color = OLD_COLOR.to_owned();
            MARK_TEXT.set(c.keyword.clone());
        }
        v.set_categories(cats, cx);
    });
    Ok(())
}

/// Прокрутка раздела «Чтение» до панели, px.
const READING_SCROLL: f32 = -560.;
/// Прежнее имя цвета: `palette::LEGACY`.
const OLD_COLOR: &str = "red";

fn meeting(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.close_all_meetings(window, cx);
    crate::prefs::update(cx, |p| p.calendar.free_color = FreeColor::default());
    m.set_section(Section::Calendar, cx);
    m.open_meeting_screen(cx);
    Ok(())
}

/// Своя встреча демо с «Изменить» и «Отменить встречу».
fn own_meeting(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.cal.update(cx, |c, cx| {
        c.set_view(V::WorkWeek, cx);
        c.go_to_demo_week(cx);
        c.select_own(false, cx);
    });
    m.set_section(Section::Calendar, cx);
    Ok(())
}

/// Раздел прокручен вниз: строки календарей коллег.
fn calendar_bottom(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    fresh(S::Calendar, m, window, cx);
    if let Some(v) = m.settings.view.clone() {
        v.update(cx, |v, cx| {
            v.scroll.scroll_to_bottom();
            cx.notify();
        });
    }
    Ok(())
}

fn sized(w: f32, h: f32, window: &mut Window) {
    window.resize(size(px(w), px(h)));
}

fn narrow(_: &Scene, _: &mut Mailer, window: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    sized(1000., 800., window);
    Ok(())
}

/// Окно — как у прочих сцен; A — клавиша схемы «Одиночные клавиши».
fn wide(_: &Scene, _: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    sized(1600., 1000., window);
    crate::prefs::update(cx, |p| p.key_scheme = postman_core::prefs::KeyScheme::Single);
    Ok(())
}

/// Правый край карточки в окне шириной 1000: поля панели и карточки.
const NARROW_EDGE: f32 = 985.;
/// Кнопка ответа сужается, но не в значок: подпись справа сжимается раньше.
const BUTTON_MIN: f32 = 60.;

/// Встреча демо — в прошлом: «сейчас» — за сутки до неё, карточка — новое приглашение.
fn new_invite(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
    m.reading.update(cx, |r, cx| {
        let card = r.reading.as_mut().and_then(|r| r.invite.as_mut());
        if let Some(c) = card.filter(|c| c.invitation().is_some() && c.clock.is_none()) {
            c.screen_state("new");
            cx.notify();
        }
    });
}

// --- точки ---

/// Строка списка.
fn row(m: &Mailer, cx: &App, i: usize) -> Option<Point<Pixels>> {
    crate::screens::row_place(m, cx, i)
}

fn row0(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, 0)
}

/// Строка письма с приглашением.
fn invite_row(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, screens::demo_row(m.list.read(cx), crate::invite_card::scenes::invitation)?)
}

/// Кнопка «×» вкладки «Настройки».
fn tab_close(m: &Mailer, w: &Window, _: &App) -> Option<Point<Pixels>> {
    let id = m.tabs.iter().find(|t| t.kind == TabKind::Settings)?.id;
    spot_center(&format!("tab-close-{id}"), w)
}

/// Пустое место сетки встречи: под строками участников.
fn grid_gap(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    let b = spot_bounds("assistant")?;
    // Правее середины: нижнюю строку слева занимает плашка «Указать ссылку», она щелчок не пропускает.
    Some(point(b.center().x + px(30.), b.origin.y + b.size.height - px(6.)))
}

/// Сколько точек на секунду сетки встречи.
fn track_scale(m: &Mailer, cx: &App) -> Option<f32> {
    let (picked, _) = meeting_view(m, cx)?.picked_range()?;
    Some(f32::from(spot_bounds("picked")?.size.width) / picked.len() as f32)
}

fn picked_center(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    spot_bounds("picked").map(|b| b.center())
}

fn shifted(p: Option<Point<Pixels>>, m: &Mailer, cx: &App, secs: f32) -> Option<Point<Pixels>> {
    let scale = track_scale(m, cx)?;
    p.map(|p| point(p.x + px(secs * scale), p.y))
}

/// На 40 мин правее: к ближайшему шагу сетки — 30 мин.
fn picked_later(m: &Mailer, w: &Window, cx: &App) -> Option<Point<Pixels>> {
    shifted(picked_center(m, w, cx), m, cx, 40. * 60.)
}

fn end_grip(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    spot_bounds("picked-end").map(|b| b.center())
}

fn end_later(m: &Mailer, w: &Window, cx: &App) -> Option<Point<Pixels>> {
    shifted(end_grip(m, w, cx), m, cx, 30. * 60.)
}

fn start_grip(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    spot_bounds("picked-start").map(|b| b.center())
}

/// Левая ручка — за правую: встреча не короче шага.
fn start_past_end(m: &Mailer, w: &Window, cx: &App) -> Option<Point<Pixels>> {
    shifted(start_grip(m, w, cx), m, cx, 5. * 3600.)
}

/// Далеко за левый край дня.
fn far_left(m: &Mailer, w: &Window, cx: &App) -> Option<Point<Pixels>> {
    shifted(picked_center(m, w, cx), m, cx, -14. * 3600.)
}

/// Первая строка списка контактов.
fn first_contact(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    screens::spot_first("contact-").map(|b| b.center())
}

/// Первая надпись «+N ещё».
fn month_more(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    screens::spot_first("month-more").map(|b| b.center())
}

// --- настройки ---

fn view<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a SettingsView> {
    m.settings.view.as_ref().map(|v| v.read(cx))
}

/// Настройки, уже записанные в файл: запись идёт в фоне.
fn saved() -> Option<Prefs> {
    Prefs::load(&crate::prefs::path(&crate::data_dir())).ok()
}

fn mail_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    rows_loaded(m.list.read(cx))
}

/// Список нарисован: точки строк известны.
fn list_drawn(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.tabs.is_active(TabKind::Main) && m.list.read(cx).bounds.get().size.width > px(0.)
}

fn settings_open(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.is_active(TabKind::Settings) && m.settings.view.is_some()
}

fn one_settings_tab(s: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_open(s, m, cx) && m.tabs.iter().filter(|t| t.kind == TabKind::Settings).count() == 1
}

fn settings_closed(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.is_active(TabKind::Main) && m.settings.view.is_none() && !m.tabs.iter().any(|t| t.kind == TabKind::Settings)
}

fn first_found(m: &Mailer, query: &str, cx: &App) -> Option<S> {
    view(m, cx).filter(|v| v.query() == query).and_then(|_| crate::settings_tab::found(query, crate::features::get(cx)).first().copied())
}

fn first_is_accounts(_: &Scene, m: &Mailer, cx: &App) -> bool {
    first_found(m, "пароль", cx) == Some(S::Accounts) // i18n-ok: ввод сценария
}

fn first_is_keys(_: &Scene, m: &Mailer, cx: &App) -> bool {
    first_found(m, "клавиши", cx) == Some(S::Keys) // i18n-ok: ввод сценария
}

fn on_accounts(_: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_on(m, S::Accounts, cx)
}

fn on_keys(_: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_on(m, S::Keys, cx)
}

fn on_general(_: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_on(m, S::General, cx)
}

fn on_rules(_: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_on(m, S::Rules, cx) && m.settings.rules_panel.is_some()
}

fn on_vacation(_: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_on(m, S::Vacation, cx) && m.settings.vacation_panel.is_some()
}

fn on_calendar_settings(_: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_on(m, S::Calendar, cx)
}

fn add_account_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    settings_on(m, S::Accounts, cx) && view(m, cx).is_some_and(|v| v.add.is_some())
}

// Категории.

fn editing_category(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.cat_edit.is_some())
}

fn not_editing_category(s: &Scene, m: &Mailer, cx: &App) -> bool {
    !editing_category(s, m, cx)
}

/// Все пять цветов заняты: щелчок по образцу — причина, цвет не меняется.
fn color_refused(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).and_then(|v| v.cat_edit.as_ref()).is_some_and(|e| e.error.is_some())
}

/// Занятая клавиша не выбирается: у новой категории — первая свободная.
fn key_refused(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).and_then(|v| v.cat_edit.as_ref()).is_some_and(|e| e.key == Some(6))
}

fn visible_categories(m: &Mailer, cx: &App) -> usize {
    view(m, cx).map_or(0, |v| v.cats.visible().count())
}

fn four_categories(_: &Scene, m: &Mailer, cx: &App) -> bool {
    visible_categories(m, cx) == 4
}

fn five_categories(_: &Scene, m: &Mailer, cx: &App) -> bool {
    visible_categories(m, cx) == 5
}

fn category_added(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).and_then(|v| v.cats.visible().find(|c| c.name == NEW_CATEGORY)).is_some_and(|c| {
        MARK_TEXT.set(c.keyword.clone());
        c.key == Some(5)
    })
}

fn row0_selected(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).selected == Some(0) && screens::reading(m, cx).is_some()
}

/// Клавиша категории поставила её на письмо: полоса в списке — на снимке.
fn row_has_new_category(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let keyword = MARK_TEXT.with_borrow(Clone::clone);
    !keyword.is_empty() && m.list.read(cx).row(0).is_some_and(|r| r.keywords.iter().any(|k| k.eq_ignore_ascii_case(&keyword)))
}

fn purge_line(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.cat_purge.is_some())
}

/// Демо: сервера нет, письма не меняются; строка подтверждения закрылась, удалённая категория на месте. Тост «нужен
/// сервер» ушёл: он лёг бы поверх поля следующей сцены.
fn purge_asked_and_closed(s: &Scene, m: &Mailer, cx: &App) -> bool {
    !purge_line(s, m, cx) && four_categories(s, m, cx) && needs_server(s, m, cx) && no_toast(s, m, cx)
}

fn old_color_renamed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let keyword = MARK_TEXT.with_borrow(Clone::clone);
    view(m, cx).and_then(|v| v.cats.list.iter().find(|c| c.keyword == keyword)).is_some_and(|c| c.color != OLD_COLOR && c.color == c.shown_color())
}

// Клавиши.

/// Таблица уже отфильтрована и нарисована: щелчок попадёт в нужную строку.
fn one_key_row(_: &Scene, _: &Mailer, _: &App) -> bool {
    spot_bounds("settings-keys-edit-0").is_some() && spot_bounds("settings-keys-edit-1").is_none()
}

fn recording_flag(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.keys.recording == Some("mail.toggle_flag"))
}

fn recording_stopped(s: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.keys.recording.is_none()) && settings_open(s, m, cx)
}

fn own_flag_keys(cx: &App) -> Vec<String> {
    crate::prefs::get(cx).keys.get("mail.toggle_flag").cloned().unwrap_or_default()
}

fn flag_on_new_key(_: &Scene, _: &Mailer, cx: &App) -> bool {
    own_flag_keys(cx).iter().any(|k| k == "ctrl-shift-g")
}

fn row0_flag(m: &Mailer, cx: &App) -> Option<bool> {
    m.list.read(cx).row(0).map(|r| r.flagged)
}

fn remember_flag(_: &Scene, m: &Mailer, cx: &App) -> bool {
    MARK.set((i64::from(row0_flag(m, cx) == Some(true)), 0));
    true
}

fn flag_changed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    row0_flag(m, cx).is_some_and(|f| i64::from(f) != MARK.get().0)
}

fn clash_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.keys.clash.as_ref().is_some_and(|(keys, _)| keys == "ctrl-r"))
}

fn conflict_listed(_: &Scene, _: &Mailer, cx: &App) -> bool {
    let set = crate::keymap::KeySet::of(&crate::prefs::get(cx));
    !crate::keymap::conflicts(crate::commands::all(), &crate::commands::SINGLE, &set).is_empty()
}

fn no_own_keys(_: &Scene, _: &Mailer, cx: &App) -> bool {
    own_flag_keys(cx).is_empty()
}

/// «Заменить»: сочетание ушло от прежней команды, конфликта нет.
fn flag_replaced_reply(s: &Scene, m: &Mailer, cx: &App) -> bool {
    own_flag_keys(cx).iter().any(|k| k == "ctrl-r") && !conflict_listed(s, m, cx)
}

fn keys_clean(s: &Scene, m: &Mailer, cx: &App) -> bool {
    let p = crate::prefs::get(cx);
    p.keys.is_empty() && p.keys_single.is_empty() && on_keys(s, m, cx)
}

// Подписи.

fn editing_signature(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.sign.edit.is_some())
}

fn signature_count(m: &Mailer, cx: &App) -> usize {
    view(m, cx).filter(|v| v.sign.edit.is_none()).map_or(0, |v| v.sign.prefs.signatures.len())
}

fn one_signature(_: &Scene, m: &Mailer, cx: &App) -> bool {
    signature_count(m, cx) == 1
}

fn two_signatures(_: &Scene, m: &Mailer, cx: &App) -> bool {
    signature_count(m, cx) == 2
}

fn defaults_set(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.sign.prefs.defaults.values().any(|d| d.new.is_some() && d.reply.is_some() && d.new != d.reply))
}

fn letter_new_signed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    compose_tab(m).is_some_and(|c| c.read(cx).model.signature == SIGN_NEW)
}

fn signature_menu_lists_both(_: &Scene, m: &Mailer, cx: &App) -> bool {
    compose_tab(m).is_some_and(|c| {
        let c = c.read(cx);
        let list = c.signatures();
        c.signature_menu_open() && [SIGN_NEW, SIGN_REPLY].iter().all(|t| list.iter().any(|s| s == t))
    })
}

fn no_letters(_: &Scene, m: &Mailer, _: &App) -> bool {
    !m.tabs.iter().any(|t| matches!(t.kind, TabKind::Compose(_)))
}

/// Ответ (вкладкой или в области чтения) — с подписью для ответов.
fn reply_signed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.compose.last().is_some_and(|c| c.read(cx).model.signature == SIGN_REPLY)
}

// Настройки календаря.

fn cal_prefs(cx: &App) -> Calendar {
    crate::prefs::get(cx).calendar
}

fn monday_off(_: &Scene, _: &Mailer, cx: &App) -> bool {
    !cal_prefs(cx).work_days[0]
}

fn start_ten(_: &Scene, _: &Mailer, cx: &App) -> bool {
    cal_prefs(cx).work_start == 10 * 60
}

fn end_five(_: &Scene, _: &Mailer, cx: &App) -> bool {
    cal_prefs(cx).work_end == 17 * 60
}

fn zone_list_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).is_some_and(|v| v.cal_menu.is_some())
}

fn zone_list_closed(s: &Scene, m: &Mailer, cx: &App) -> bool {
    !zone_list_open(s, m, cx) && settings_open(s, m, cx)
}

/// Неделя пересчитана по новым дням и часам: затенение — на снимке, «Предпросмотр» пишет те же значения.
fn week_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = cal_prefs(cx);
    m.section == Section::Calendar
        && m.tabs.is_active(TabKind::Main)
        && c.work_days.iter().filter(|d| **d).count() == 4
        && c.work_start == 600
        && m.cal.read(cx).view == V::Week
}

fn zone_ural(_: &Scene, _: &Mailer, cx: &App) -> bool {
    cal_prefs(cx).zone.as_deref() == Some(URAL)
}

fn zone_windows(_: &Scene, _: &Mailer, cx: &App) -> bool {
    cal_prefs(cx).zone.is_none()
}

fn calendar_in_ural(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.section == Section::Calendar && m.tabs.is_active(TabKind::Main) && m.cal.read(cx).zone_name() == Some(URAL)
}

fn first_sunday(_: &Scene, _: &Mailer, cx: &App) -> bool {
    cal_prefs(cx).week_start == postman_core::prefs::WeekStart::Sunday
}

fn view_chosen(_: &Scene, _: &Mailer, cx: &App) -> bool {
    cal_prefs(cx).view == V::Week
}

fn step_quarter(_: &Scene, _: &Mailer, cx: &App) -> bool {
    cal_prefs(cx).step == 15
}

fn calendar_chosen(c: &Calendar) -> bool {
    c.zone.as_deref() == Some(URAL) && c.step == 15 && c.view == V::Week && c.second_zone_on
}

fn zone_saved(_: &Scene, _: &Mailer, cx: &App) -> bool {
    calendar_chosen(&cal_prefs(cx)) && saved().is_some_and(|p| calendar_chosen(&p.calendar))
}

fn calendar_default(_: &Scene, _: &Mailer, cx: &App) -> bool {
    let c = cal_prefs(cx);
    let d = Calendar::default();
    c.work_days == d.work_days && c.work_start == d.work_start && c.zone == d.zone && c.view == d.view
}

// Чтение.

fn pane_is(cx: &App, pane: Pane) -> bool {
    crate::prefs::get(cx).reading.pane == pane
}

fn pane_bottom(_: &Scene, _: &Mailer, cx: &App) -> bool {
    pane_is(cx, Pane::Bottom)
}

fn pane_hidden(_: &Scene, _: &Mailer, cx: &App) -> bool {
    pane_is(cx, Pane::Hidden)
}

fn letter_instead_of_list(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.reading_instead_of_list(cx)
}

fn list_back(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !m.reading_instead_of_list(cx) && m.tabs.is_active(TabKind::Main)
}

fn pane_saved(s: &Scene, m: &Mailer, cx: &App) -> bool {
    list_back(s, m, cx) && saved().is_some_and(|p| p.reading.pane == Pane::Hidden)
}

// Внешний вид.

fn is_dark(cx: &App) -> bool {
    Themes::try_get(cx).is_some_and(|t| t.state().effective() == FLUENT_DARK)
}

fn dark(_: &Scene, _: &Mailer, cx: &App) -> bool {
    is_dark(cx)
}

fn compact(_: &Scene, _: &Mailer, cx: &App) -> bool {
    crate::prefs::get(cx).general.density == Some(20) && crate::roles::look(cx).density().row() == 20
}

fn english(_: &Scene, _: &Mailer, cx: &App) -> bool {
    tr!("settings-nav-general") == "General" && crate::prefs::get(cx).general.language.as_deref() == Some("en")
}

fn look_in_file() -> bool {
    saved().is_some_and(|p| {
        p.general.theme.as_deref() == Some(FLUENT_DARK) && p.general.density == Some(20) && p.general.language.as_deref() == Some("en")
    })
}

fn look_saved(s: &Scene, m: &Mailer, cx: &App) -> bool {
    english(s, m, cx) && is_dark(cx) && look_in_file()
}

/// Второй запуск: тема, плотность, язык, панель и настройки календаря — из файла; часы — из `cal-36-hours`.
fn all_kept(s: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = cal_prefs(cx);
    look_saved(s, m, cx)
        && compact(s, m, cx)
        && pane_is(cx, Pane::Hidden)
        && calendar_chosen(&c)
        && !c.work_days[0]
        && c.work_start == 600
        && c.work_end == 1020
        && m.cal.read(cx).zone_name() == Some(URAL)
        && m.cal.read(cx).view == V::Week
}

// Встреча.

/// Ответ сервера о занятости приходит как на живом ящике: через `Mailer::on_sync`, пока `Mailer` обновляется.
const SERVER_BUSY: &[Input] = &[
    Input::Until(|_, m, _| m.meetings.last().is_some()),
    Input::Run(|_, m, _, cx| {
        let view = m.meetings.last().ok_or("нет вкладки встречи")?;
        let ticket = view.update(cx, |v, _| v.await_server_busy());
        let busy = postman_calendar::Busy { span: postman_calendar::Span::new(0, 3600), kind: postman_calendar::BusyKind::Busy };
        let ev = postman_core::sync::Event::Availability { ticket, me: vec![busy], people: Vec::new(), notes: Vec::new(), error: None };
        m.on_sync(&ev, cx);
        Ok(())
    }),
];

fn server_busy_taken(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(MeetingView::server_busy_taken)
}

fn meeting_view<'a>(m: &Mailer, cx: &'a App) -> Option<&'a MeetingView> {
    m.meetings.last().map(|v| v.read(cx))
}

fn meeting_tab(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.is_active(TabKind::Meeting)
}

fn no_meeting(_: &Scene, m: &Mailer, _: &App) -> bool {
    !m.tabs.iter().any(|t| t.kind == TabKind::Meeting)
}

fn meeting_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_tab(s, m, cx) && meeting_view(m, cx).is_some_and(|v| v.ready() && !v.free_starts().is_empty()) && spot_bounds("assistant").is_some()
}

fn cursor(m: &Mailer, cx: &App) -> Option<(i64, Vec<i64>)> {
    meeting_view(m, cx).and_then(|v| v.slot_cursor.map(|c| (c, v.free_starts())))
}

fn cursor_first(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cursor(m, cx).is_some_and(|(c, runs)| runs.first() == Some(&c))
}

fn cursor_second(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cursor(m, cx).is_some_and(|(c, runs)| runs.get(1) == Some(&c))
}

fn cursor_last(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cursor(m, cx).is_some_and(|(c, runs)| runs.last() == Some(&c))
}

fn cursor_on_free(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cursor(m, cx).is_some_and(|(c, runs)| runs.contains(&c))
}

fn picked_cursor(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.slot_cursor.is_some() && v.picked_range().map(|(p, _)| p.start) == v.slot_cursor)
}

/// В поле ←/→ — курсор текста («aXb»), Enter окна не трогает.
fn title_typed(s: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.title_text(cx).ends_with("aXb")) && picked_cursor(s, m, cx) && cursor_last(s, m, cx)
}

fn only_long_windows(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.model.duration == 7200 && v.shortest_free().is_some_and(|s| s >= 7200))
}

fn picked(m: &Mailer, cx: &App) -> Option<(i64, i64)> {
    meeting_view(m, cx).and_then(|v| v.picked_range()).map(|(p, _)| (p.start, p.end))
}

fn remember_picked(_: &Scene, m: &Mailer, cx: &App) -> bool {
    picked(m, cx).is_some_and(|p| {
        MARK.set(p);
        spot_bounds("picked").is_some()
    })
}

/// Поля следуют за рамкой: «Начало» и длительность — из того же выбранного времени.
fn fields_follow(m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.picked_range().is_some_and(|(p, _)| p.len() == v.model.duration))
}

fn moved_half_hour(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let (a, b) = MARK.get();
    picked(m, cx) == Some((a + 1800, b + 1800)) && fields_follow(m, cx)
}

fn end_half_hour_later(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let (a, b) = MARK.get();
    picked(m, cx) == Some((a, b + 1800)) && fields_follow(m, cx)
}

fn shortest_span(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.picked_range().is_some_and(|(p, _)| p.len() == v.grid_step() && p.end == MARK.get().1))
}

fn at_day_start(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.picked_range().is_some_and(|(p, range)| p.start == range.start))
}

/// Shift+→: конец — на шаг сетки.
fn widened(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let (a, b) = MARK.get();
    meeting_view(m, cx).is_some_and(|v| picked(m, cx) == Some((a, b + v.grid_step())))
}

fn date_text(m: &Mailer, cx: &App) -> String {
    meeting_view(m, cx).map(|v| v.model.date.clone()).unwrap_or_default()
}

fn remember_date(_: &Scene, m: &Mailer, cx: &App) -> bool {
    MARK_TEXT.set(date_text(m, cx));
    true
}

fn date_changed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    MARK_TEXT.with_borrow(|d| !d.is_empty() && *d != date_text(m, cx))
}

fn date_same(_: &Scene, m: &Mailer, cx: &App) -> bool {
    MARK_TEXT.with_borrow(|d| *d == date_text(m, cx))
}

fn date_today(_: &Scene, m: &Mailer, cx: &App) -> bool {
    date_text(m, cx) == jiff::Zoned::now().strftime("%d.%m.%Y").to_string()
}

/// В поле PgDn — не день: тот же день, фокус — в поле.
fn date_kept(s: &Scene, m: &Mailer, cx: &App) -> bool {
    date_same(s, m, cx) && date_today(s, m, cx)
}

fn tab_named(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.tabs.active().kind == TabKind::Meeting && m.meetings.title(m.tabs.active().id, cx).contains(NEW_CATEGORY) && !m.tabs.active().kind.shows_mail()
}

fn one_chip(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.chips().0 == 1)
}

fn two_chips(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.chips().0 == 2)
}

fn suggests_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(|v| v.suggestions().0 > 0)
}

// Контакты.

fn no_toast(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !m.toasts.read(cx).toast_shown()
}

fn contacts_open(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.section == Section::Contacts
}

fn contact_row_shown(_: &Scene, _: &Mailer, _: &App) -> bool {
    screens::spot_first("contact-").is_some()
}

fn contact_picked(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.contacts.read(cx).selected_email().is_some()
}

fn contact_form(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.contacts.read(cx).editing()
}

fn no_contact_form(s: &Scene, m: &Mailer, cx: &App) -> bool {
    !contact_form(s, m, cx)
}

fn has_drill_contact(m: &Mailer, cx: &App) -> bool {
    m.contacts.read(cx).personal_emails().iter().any(|e| e == CONTACT_MAIL)
}

fn drill_contact_there(s: &Scene, m: &Mailer, cx: &App) -> bool {
    has_drill_contact(m, cx) && no_contact_form(s, m, cx)
}

fn drill_contact_gone(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !has_drill_contact(m, cx) && m.section == Section::Contacts
}

/// Адресная книга собирается в фоне, подсказки считаются при наборе.
fn book_ready(_: &Scene, m: &Mailer, _: &App) -> bool {
    !m.compose.book.borrow().suggest("ива", 1, 0).is_empty() // i18n-ok: ввод сценария
}

fn letter_open(_: &Scene, m: &Mailer, _: &App) -> bool {
    compose_tab(m).is_some()
}

fn letter_to_contact(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let selected = m.contacts.read(cx).selected_email();
    compose_tab(m).is_some_and(|c| selected.is_some_and(|e| c.read(cx).model.field(Field::To).contains(&e)))
}

fn letter_suggests(_: &Scene, m: &Mailer, cx: &App) -> bool {
    compose_tab(m).is_some_and(|c| c.read(cx).suggestions() > 0)
}

/// Найдена Иванова и поиск идёт по этому запросу.
fn found(m: &Mailer, query: &str, cx: &App) -> bool {
    let c = m.contacts.read(cx);
    c.query() == query && c.found_groups() > 0 && c.found_emails().iter().any(|e| e == IVANOVA)
}

fn found_cyrillic(_: &Scene, m: &Mailer, cx: &App) -> bool {
    found(m, "ива", cx) // i18n-ok: ввод сценария
}

fn found_layout(_: &Scene, m: &Mailer, cx: &App) -> bool {
    found(m, "bdf", cx)
}

fn same_people(_: &Scene, m: &Mailer, cx: &App) -> bool {
    found(m, "ivanova", cx)
}

// Календарь.

fn in_calendar(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.section == Section::Calendar
}

fn cal_view(m: &Mailer, cx: &App) -> V {
    m.cal.read(cx).view
}

fn view_day(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cal_view(m, cx) == V::Day
}

fn view_work_week(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cal_view(m, cx) == V::WorkWeek
}

fn view_week(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cal_view(m, cx) == V::Week
}

fn view_month(_: &Scene, m: &Mailer, cx: &App) -> bool {
    cal_view(m, cx) == V::Month
}

fn first_day(m: &Mailer, cx: &App) -> Option<jiff::civil::Date> {
    m.cal.read(cx).days().first().copied()
}

fn remember_day(_: &Scene, m: &Mailer, cx: &App) -> bool {
    MARK_TEXT.set(first_day(m, cx).map(|d| d.to_string()).unwrap_or_default());
    true
}

fn day_after(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let text = MARK_TEXT.with_borrow(Clone::clone);
    first_day(m, cx).is_some_and(|d| d.yesterday().is_ok_and(|y| y.to_string() == text))
}

fn day_same(_: &Scene, m: &Mailer, cx: &App) -> bool {
    MARK_TEXT.with_borrow(|t| first_day(m, cx).is_some_and(|d| d.to_string() == *t))
}

fn day_today(_: &Scene, m: &Mailer, cx: &App) -> bool {
    first_day(m, cx) == Some(jiff::Zoned::now().date())
}

fn other_month(s: &Scene, m: &Mailer, cx: &App) -> bool {
    !day_same(s, m, cx)
}

fn month_more_shown(_: &Scene, _: &Mailer, _: &App) -> bool {
    screens::spot_first("month-more").is_some()
}

/// «+N ещё» — к этому дню в виде «День».
fn month_more_opened(s: &Scene, m: &Mailer, cx: &App) -> bool {
    view_day(s, m, cx)
}

fn found_events(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    c.results_ready() && c.result_state().0 > 1
}

fn second_result(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.cal.read(cx).result_state().1 == 1
}

fn first_result(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.cal.read(cx).result_state().1 == 0
}

fn event_opened(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.cal.read(cx).selected()
}

fn search_cleared(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.cal.read(cx).result_state().0 == 0 && m.cal.read(cx).selected()
}

fn subscription_refused(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.cal.read(cx).subscription_refused()
}

fn link_refused(_: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_view(m, cx).is_some_and(MeetingView::link_refused)
}

fn editing_meeting(s: &Scene, m: &Mailer, cx: &App) -> bool {
    meeting_tab(s, m, cx) && meeting_view(m, cx).is_some_and(|v| v.model.editing.is_some())
}

/// В истории уведомлений — «Нужна учётная запись на сервере».
fn needs_server(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let text = tr!("needs-server");
    m.toasts.read(cx).stack_history().any(|e| e.text.as_ref() == text)
}

/// Новый тост после шага с клавишей.
fn answered_again(s: &Scene, m: &Mailer, cx: &App) -> bool {
    needs_server(s, m, cx) && m.toasts.read(cx).stack_history().count() as i64 > MARK.get().0
}

/// Дальше считается только новый тост: прошлые — из истории.
fn forget_toasts(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let seen = m.toasts.read(cx).stack_history().count();
    MARK.set((seen as i64, 0));
    true
}

fn invite_card(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).and_then(|r| r.invite.as_ref()).is_some_and(|c| c.clock.is_some()) && spot_bounds("invite-accept").is_some()
}

/// Кнопки — в ряд и в пределах окна; подпись справа сужается.
fn invite_fits(s: &Scene, m: &Mailer, cx: &App) -> bool {
    invite_card(s, m, cx)
        && spot_bounds("invite-decline").is_some_and(|b| {
            let accept = spot_bounds("invite-accept");
            let wide_enough = accept.is_some_and(|a| a.size.width >= px(BUTTON_MIN));
            f32::from(b.origin.x + b.size.width) < NARROW_EDGE && accept.is_some_and(|a| b.origin.x > a.origin.x) && wide_enough
        })
}

fn colleague_hidden(_: &Scene, _: &Mailer, cx: &App) -> bool {
    !cal_prefs(cx).hidden_calendars.is_empty()
}

fn colleagues_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.section == Section::Calendar && cal_prefs(cx).hidden_calendars.is_empty()
}

// Календари рядом (T3-05).

/// Раздел «Календарь», демо-неделя видом «Раб. неделя», «сейчас» — 14:20 сегодня.
fn one_colleague(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.close_all_meetings(window, cx);
    window.focus(&m.focus, cx);
    m.set_section(Section::Calendar, cx);
    let clock = jiff::Zoned::now().date().at(14, 20, 0, 0).to_zoned(jiff::tz::TimeZone::system()).ok().map(|z| z.timestamp().as_second());
    m.cal.update(cx, |c, cx| {
        c.clock = clock;
        c.set_view(V::WorkWeek, cx);
        c.go_to_demo_week(cx);
        c.close_details(cx);
    });
    Ok(())
}

/// Как `start`, и снова раздел «Календарь»: прогон начинает сцену с почты. Вид и неделя — прошлой сцены.
fn in_cal(s: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    start(s, m, window, cx)?;
    m.set_section(Section::Calendar, cx);
    Ok(())
}

/// Флажок — только у календаря Ивановой; режим — наложением.
fn only_ivanova(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    let hidden: Vec<String> = m.cal.read(cx).colleagues.iter().map(|o| o.key()).filter(|k| k != IVANOVA).collect();
    if hidden.is_empty() {
        return Err("в демо нет календарей коллег".into());
    }
    crate::prefs::update(cx, move |p| {
        p.calendar.hidden_calendars = hidden;
        p.calendar.side_by_side = false;
    });
    Ok(())
}

fn ivanova_only(m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    let hidden = cal_prefs(cx).hidden_calendars;
    !c.colleagues.is_empty() && c.colleagues.iter().all(|o| hidden.contains(&o.key()) == (o.key() != IVANOVA))
}

fn overlay_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    m.section == Section::Calendar && ivanova_only(m, cx) && c.view == V::WorkWeek && !c.side_by_side() && c.side_count() == 1
}

fn side_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    m.section == Section::Calendar
        && ivanova_only(m, cx)
        && c.view == V::WorkWeek
        && c.side_by_side()
        && cal_prefs(cx).side_by_side
        && c.side_count() == 2
}

fn side_day(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    c.view == V::Day && c.side_by_side() && c.side_count() == 2
}

fn overlay_back(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    c.view == V::WorkWeek && !c.side_by_side() && !cal_prefs(cx).side_by_side && c.side_count() == 1
}

fn palette_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() > 0)
}

fn palette_one(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() == 1)
}

/// Снова рядом — и в файле настроек.
fn side_saved(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.is_none() && m.cal.read(cx).side_by_side() && m.cal.read(cx).side_count() == 2 && saved().is_some_and(|p| p.calendar.side_by_side)
}

fn side_kept(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = m.cal.read(cx);
    m.section == Section::Calendar && c.side_by_side() && c.side_count() == 2 && c.view == V::WorkWeek
}

fn owner_colors_off(_: &Scene, _: &Mailer, cx: &App) -> bool {
    !cal_prefs(cx).owner_colors
}

fn owner_names_off(_: &Scene, _: &Mailer, cx: &App) -> bool {
    !cal_prefs(cx).owner_names
}

fn owner_off_in_week(s: &Scene, m: &Mailer, cx: &App) -> bool {
    owner_colors_off(s, m, cx) && owner_names_off(s, m, cx) && m.section == Section::Calendar && m.tabs.is_active(TabKind::Main)
}
