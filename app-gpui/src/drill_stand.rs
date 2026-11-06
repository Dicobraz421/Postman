//! Сценарии с вводом на живых ящиках стенда: шаги класса `stand` (`docs/manual-tests/`, `docs/MANUAL-TESTS.md`), которым нужны
//! сервер и вторая сторона. Коллегу и проверку состояния на сервере играет `docs/stand/peer.py`: он запускает Postman с нужной
//! учётной записью (`POSTMAN_URL`…) и по строкам `screen …` шлёт письма, меняет ящик и сверяет результат запросами JMAP, IMAP,
//! CalDAV и CardDAV. `ready` сцены проверяет обещанное шагом по модели окна, вид — снимок.
//!
//! Метка прогона `POSTMAN_STAND_STAMP` — в темах писем коллеги: так сцена находит своё письмо среди писем прошлых прогонов.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::EmailRow;
use postman_calendar::Task;
use postman_calendar::assign::Status;
use postman_core::assign::Tab;
use postman_core::model::MigrationState;
use postman_core::rules::Action;
use postman_core::tasks::List;

use crate::Mailer;
use crate::rules::{Kind, RulesPanel};
use crate::screens::{self, Held, Input, Scene, rows_loaded, settings_on};
use crate::settings_tab::Section as Page;
use crate::tasks::Nav;
use crate::vacation::VacationPanel;

pub(crate) const PART: crate::part::Part = crate::part::Part { name: "drill-stand", scenes: SCENES, ..crate::part::Part::NONE };

/// Шаг со своим условием.
macro_rules! until {
    (|$m:tt, $cx:tt| $done:expr) => {
        Input::Until(|_, $m, $cx| $done)
    };
}

const fn scene(name: &'static str, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, start, ready).input(input).live().on_request()
}

const fn more(name: &'static str, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, start, ready).input(input).live().on_request().continues()
}

/// Письмо с файлом: открыть и нажать первую карточку вложения.
macro_rules! file_letter {
    ($word:expr) => {
        &[
            Input::Until(mail_ready),
            until!(|m, cx| placed(m, cx, $word).is_some()),
            Input::Click(|m, _, cx| placed(m, cx, $word), Held::No),
            until!(|m, cx| picked(m, cx, $word).is_some()),
            Input::Press("att-card-0"),
        ]
    };
}

const SCENES: &[Scene] = &[
    // S5-26, шаг 2: правило «Тема содержит тендер → категория». Без выбора категории «Сохранить» отказывает.
    scene("st-s526-rule", |_, m, cx| rules_count(m, cx) == Some(1), RULE_CATEGORY),
    // S5-26, шаг 2: письмо пришло с меткой (ключевое слово — на сервере, сверяет `peer.py`).
    scene("st-s526-label", |_, m, cx| picked(m, cx, FIRST_TENDER).is_some_and(|r| has_tenders(m, cx, r)), LABEL),
    // S5-26, шаг 3: правило удалено.
    scene("st-s526-gone", |_, m, cx| rules_count(m, cx) == Some(0), DELETE_RULE),
    // S5-26, шаг 3: следующее письмо без метки.
    scene("st-s526-plain", |_, m, cx| picked(m, cx, NEXT_TENDER).is_some_and(|r| !has_tenders(m, cx, r)), PLAIN),
    // Раздел 16, шаг 2: правило «Тема содержит «проект» → в «Проекты»».
    scene("st-16-move", |_, m, cx| rules_count(m, cx) == Some(1), RULE_MOVE),
    // Раздел 16, шаг 3: письмо в «Проектах».
    scene("st-16-moved", |_, m, cx| in_folder(m, cx, PROJECTS) && find(m, cx, ALPHA).is_some(), MOVED),
    // Раздел 16, шаг 3: флажок правила снят.
    scene("st-16-off", |_, m, cx| first_rule_on(m, cx) == Some(false), RULE_OFF),
    // Раздел 16, шаг 3: письмо во «Входящих».
    scene("st-16-inbox", |_, m, cx| find(m, cx, BETA).is_some(), BACK_IN_INBOX),
    // Раздел 16, шаг 4: адрес пересылки без «@» — ошибка в редакторе.
    scene("st-16-address", |_, m, cx| rules(m, cx).is_some_and(|p| p.editor.is_none()), BAD_ADDRESS),
    // Раздел 16, шаг 5: «Удалить» → «Удалить правило?» → правила нет.
    scene("st-16-delete", |_, m, cx| rules_count(m, cx) == Some(0), DELETE_RULE),
    // Раздел 16, шаг 7: чужой скрипт виден текстом, правка спрашивает «Заменить», «Отмена» ничего не меняет.
    scene("st-16-foreign", |_, m, cx| rules(m, cx).is_some_and(|p| p.conflict.is_none() && p.editor.is_some()), FOREIGN),
    // S5-29, шаг 2: «Ж» вокруг выделенного слова, «Есть несохранённые изменения».
    scene("st-s529-format", |_, m, cx| vacation(m, cx).is_some_and(|p| p.dirty() && p.form.reply.internal.body == BOLD_WORD), FORMAT),
    // S5-29, шаг 2: «Отмена» — как было.
    more("st-s529-cancel", |_, m, cx| vacation(m, cx).is_some_and(|p| !p.dirty() && p.form.reply.internal.body.is_empty()), CANCEL),
    // S5-29, шаг 3: «Внешним»: снят флажок «Отвечать только контактам».
    more("st-s529-external", |_, m, cx| vacation(m, cx).is_some_and(|p| !p.form.reply.contacts_only), EXTERNAL),
    // Раздел 15, шаг 4: «по» раньше «с» — ошибка в панели.
    more("st-15-order", |_, m, cx| vacation(m, cx).is_some_and(|p| p.error.is_some()), BAD_ORDER),
    // S5-29, шаг 4: включён с периодом, темы разные, «Вне офиса»: сохранено на сервере, полоса.
    more("st-s529-save", vacation_saved, SAVE_VACATION),
    // Раздел 15, шаг 6, и S5-29, шаг 6: перезапуск: полоса сразу после входа, тексты и флажки из скрипта.
    scene("st-s529-kept", kept, KEPT),
    // S5-29, шаг 7: «Выключить» — тост с «Отменить»; «Отменить» — снова включён.
    scene("st-s529-undo", |_, m, _| m.settings.bar.is_some(), TURN_OFF_UNDO),
    // S5-29, шаг 7, и раздел 15, шаг 5: выключен совсем.
    scene("st-s529-off", |_, m, _| m.settings.bar.is_none(), TURN_OFF),
    // S5-29, шаг 8, и раздел 15, шаг 7: занято больше 90 %: индикатор и тост; щелчок по индикатору — тост снова.
    scene("st-s529-quota", quota_toast, QUOTA),
    // S5-29, шаг 8: «Очистить вложения…» — поиск «Везде».
    more("st-s529-clean", |_, m, cx| m.search.read(cx).value().contains("has:attachment"), CLEAN),
    // S6-05, шаг 7: задача из Postman: на сервере VTODO, сверяет `peer.py`; он же правит её и заводит новую по CalDAV.
    scene("st-s605-new", |_, m, cx| task(m, cx, FIRST_TASK).is_some(), NEW_TASK),
    // S6-05, шаг 7: правка с другого клиента и новая задача появились сами (push календаря).
    scene("st-s605-push", pushed_tasks, TASKS_PUSHED),
    // S6-06, шаг 4: «!» в карточке: поля VTODO сверяет `peer.py`, второе окно видит правку.
    scene("st-s606-card", |_, m, cx| task(m, cx, EDITED_TASK).is_some_and(|t| t.important), IMPORTANT),
    // S6-06, шаг 4: второй Postman открыт раньше правки.
    scene("st-s606-open", tasks_ready, &[Input::Until(mail_ready), OPEN_TASKS, Input::Until(tasks_ready)]),
    // S6-06, шаг 4: второй Postman: правка пришла сама.
    more("st-s606-seen", |_, m, cx| task(m, cx, EDITED_TASK).is_some_and(|t| t.important), SECOND_WINDOW),
    // S6-08, шаг 2: блок поставлен, рядом «Убрать».
    scene("st-s608-block", |_, m, cx| task(m, cx, EDITED_TASK).is_some_and(|t| t.block.is_some()), PLACE_BLOCK),
    // S6-08, шаг 3: в календаре событие с названием задачи.
    scene("st-s608-week", |_, m, cx| event_shown(m, cx, &titled(EDITED_TASK)), CALENDAR_KEY),
    // S6-08, шаг 3: «Убрать» — события нет.
    scene("st-s608-gone", |_, m, cx| task(m, cx, EDITED_TASK).is_some_and(|t| t.block.is_none()), REMOVE_BLOCK),
    // S6-05, шаг 7: выполнение: на сервере `STATUS:COMPLETED`.
    scene("st-s605-done", |_, m, cx| task(m, cx, EDITED_TASK).is_some_and(|t| t.progress.is_closed()), COMPLETE),
    // S6-05, шаг 7: удаление: на сервере задачи нет.
    scene("st-s605-delete", |_, m, cx| task(m, cx, SERVER_TASK).is_none(), DELETE_TASK),
    // S6-09, шаг 8: user1 поручает user2, наблюдатель user3.
    scene("st-s609-assign", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Waiting), ASSIGN),
    // S6-09, шаг 8: у user1 статус меняется сам; «Принять результат».
    scene("st-s609-accept", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Closed), ACCEPT_RESULT),
    // S6-09, шаг 8: у user2 задача пришла, «Принять».
    scene("st-s609-got", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Accepted), TAKE_ORDER),
    // S6-09, шаг 8: у user2 «В работе», «Готово».
    more("st-s609-work", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Done), WORK_ORDER),
    // S6-09, шаг 8: у user2 «Результат принят».
    more("st-s609-closed", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Closed), ORDER_CLOSED),
    // S6-11, шаг 4: «Напомнить всем», второй раз — «Всем уже напомнили сегодня».
    scene("st-s611-remind", |_, m, cx| toast_is(m, cx, REMINDED_ALREADY), REMIND_ALL),
    // S6-11, шаг 5: «Забрать задачу» и «Отменить»: поручение осталось.
    scene("st-s611-undo", |_, m, cx| status_of(m, cx, SECOND_ORDER) == Some(Status::Waiting), REVOKE_UNDO),
    // S6-11, шаг 5: без «Отменить»: задача снова обычная.
    more("st-s611-revoke", |_, m, cx| task(m, cx, SECOND_ORDER).is_some_and(|t| postman_calendar::assign::status(t).is_none()), REVOKE),
    // S6-11, шаг 8: закрытое поручение ушло из «Ждём».
    scene("st-s611-wait", no_closed_in_wait, OPEN_WAIT),
    // Раздел 18, шаг 1: ветки обоих общих ящиков в дереве.
    scene("st-18-tree", both_shared, SHARED_TREE),
    // Раздел 18, шаг 2: письмо появилось без перезапуска.
    scene("st-18-push", |_, m, cx| find(m, cx, TO_SHARED).is_some(), SHARED_PUSH),
    // S5-28, шаг 8: первый сотрудник берёт письмо.
    scene("st-s528-take", |_, m, cx| taken_by(m, cx, &colleague("POSTMAN_USER")), TAKE),
    // S5-28, шаг 8: второй сотрудник видит «Взято в работу» и метку.
    scene("st-s528-sees", |_, m, cx| taken_by(m, cx, &colleague("POSTMAN_STAND_PEER")), SEES_TAKEN),
    // S5-28, шаг 8: второй забирает себе.
    more("st-s528-steal", |_, m, cx| taken_by(m, cx, &colleague("POSTMAN_USER")), STEAL),
    // S5-28, шаг 8: у первого «Назначено: второй».
    more("st-s528-lost", |_, m, cx| taken_by(m, cx, &colleague("POSTMAN_STAND_PEER")), LOST),
    // Раздел 18, шаг 4: ответ из ветки — «От» ящика.
    scene("st-18-reply", |_, m, cx| m.compose.inline.is_none() && find(m, cx, TO_SHARED).is_some(), REPLY_SHARED),
    // Раздел 18, шаг 3: `shared-ro`: Delete — тост, письмо на месте.
    scene("st-18-ro", |_, m, cx| find(m, cx, TO_READONLY).is_some() && m.toasts.read(cx).latest().is_some(), READ_ONLY),
    // Раздел 18, шаг 5: флаг и удаление в ящике группы.
    scene("st-18-flag", |_, m, cx| picked(m, cx, TO_SHARED).is_some_and(|r| r.flagged), FLAG_AND_DELETE),
    more("st-18-delete", |_, m, cx| find(m, cx, TO_SHARED).is_none(), DELETE_SHARED),
    // S7-03б, шаг 1: неверный пароль — причина в карточке, учётная запись не добавлена.
    scene("st-s703-wrong", |_, m, cx| add_panel(m, cx).is_some_and(|(busy, error)| !busy && error) && extra_count(m) == 0, &ADD_WRONG),
    // S7-03б, шаг 1: тот же `user1` — «Эта учётная запись уже подключена».
    scene("st-s703-same", |_, m, cx| add_error(m, cx).is_some_and(|e| e.contains(&tr!("settings-account-exists"))), SAME_ACCOUNT),
    // S7-03б, шаг 1: верный пароль («Не хранить»): карточка закрылась, вторая учётная запись в списке.
    scene("st-s703-add", |_, m, _| m.settings.add.is_none() && extra_count(m) == 1, &ADD_RIGHT),
    // S7-03б, шаг 2: письмо в папке `user2` открывается и отмечается прочитанным.
    scene("st-s703-letter", |_, m, cx| picked(m, cx, TO_SECOND).is_some_and(|r| r.seen), SECOND_LETTER),
    // S7-03б, шаг 3: ответ на письмо из папки `user2` — от `user2`.
    more("st-s703-reply", |_, m, _| m.compose.inline.is_none(), SECOND_REPLY),
    // S7-03б, шаг 2: перенос.
    scene("st-s703-move", |_, m, cx| find(m, cx, TO_SECOND).is_none(), SECOND_MOVE),
    // S7-03б, шаг 2: удаление.
    scene("st-s703-delete", |_, m, cx| find(m, cx, DELETE_SECOND).is_none(), SECOND_DELETE),
    // S7-03б, шаг 3: письмо от `user2`.
    scene("st-s703-from", |_, m, _| screens::compose_tab(m).is_none(), SEND_FROM_SECOND),
    // S7-03б, шаг 4: календарь `user2`: его событие в неделе.
    scene("st-s703-cal", |_, m, cx| event_shown(m, cx, &titled(SECOND_EVENT)), SECOND_CALENDAR),
    // S7-03б, шаг 5: встреча от `user2` с приглашённым `user1`: организатор `user2`, встреча — один раз.
    scene(
        "st-s703-meeting",
        |_, m, cx| m.section == crate::Section::Calendar && m.cal.read(cx).title_count(&titled(BOTH_MEETING)) == 1,
        MEETING_FROM_SECOND,
    ),
    // S7-03б, шаг 6: «Все папки» — письма обеих учётных записей одним списком.
    scene("st-s703-search", |_, m, cx| found_both(m, cx), SEARCH_BOTH),
    // S7-03б, шаг 8: «Удалить»: группа и адрес ушли.
    scene("st-s703-remove", second_gone, REMOVE_SECOND),
    // Раздел 14, шаг 2: число непрочитанных падает после открытия.
    scene("st-14-read", |_, m, cx| picked(m, cx, UNREAD_LETTER).is_some_and(|r| r.seen), UNREAD_FELL),
    // Раздел 14, шаг 2: и растёт после «Не прочитано».
    more("st-14-unread", |_, m, cx| unread_in_inbox(m, cx) == Some(UNREAD_BEFORE.load(std::sync::atomic::Ordering::Relaxed)), UNREAD_ROSE),
    // Раздел 14, шаг 6: неделя открыта, с другого клиента создана встреча.
    scene("st-14-week", |_, m, cx| event_shown(m, cx, &titled(WEEK_EVENT)), WEEK_ADDED),
    // Раздел 14, шаг 6: и удалена.
    more("st-14-week-gone", |_, m, cx| m.section == crate::Section::Calendar && !m.cal.read(cx).has_title(&titled(WEEK_EVENT)), WEEK_DELETED),
    // Раздел 14, шаг 4: пароль сменили при открытом окне: строка входа при следующей сверке.
    scene("st-14-password", |_, m, _| m.live.signin.is_some(), &[Input::Until(mail_ready)]),
    // Раздел 34, шаг 2: подсказки «Кому» — с ФИО.
    scene("st-34-to", typed_directory, DIRECTORY_NAMES),
    // Раздел 34, шаг 2: участник встречи из справочника — с ФИО на чипе.
    scene("st-34-chip", meeting_chip, MEETING_NAMES),
    // S7-04, шаги 1, 2: дерево папок IMAP, рейл только почты, в настройках нет календаря, правил, автоответа.
    scene("st-s704-tree", imap_tree, &[Input::Until(mail_ready)]),
    // S7-04, шаг 3: письмо с вложением открывается.
    scene("st-s704-open", |_, m, cx| picked(m, cx, WITH_FILE).is_some(), IMAP_FILE),
    // S7-04, шаг 4: письмо себе с вложением: копия в «Sent Items», новое появилось само (IDLE).
    scene("st-s704-send", |_, m, cx| find(m, cx, TO_MYSELF).is_some(), IMAP_SEND),
    // S7-04, шаг 5: прочитано, флаг, категория — на сервере (`peer.py`, IMAP).
    scene("st-s704-flags", |_, m, cx| picked(m, cx, TO_MYSELF).is_some_and(|r| r.flagged && r.seen), IMAP_FLAGS),
    // S7-04, шаг 5: флаг снят в другом клиенте: приходит сам.
    more("st-s704-flag-back", |_, m, cx| picked(m, cx, TO_MYSELF).is_some_and(|r| !r.flagged), IMAP_FLAG_BACK),
    // S7-04, шаг 6: перенос в папку.
    more("st-s704-move", |_, m, cx| find(m, cx, TO_MYSELF).is_none(), IMAP_MOVE),
    // S7-04, шаг 6: «Удалить» — в «Deleted Items».
    scene("st-s704-delete", |_, m, cx| find(m, cx, WITH_FILE).is_none(), IMAP_DELETE),
    // S7-04, шаг 6: удаление насовсем.
    scene("st-s704-purge", |_, m, cx| find(m, cx, WITH_FILE).is_none(), IMAP_PURGE),
    // S7-04, шаг 7: две версии черновика: на сервере одна копия.
    scene("st-s704-draft", |_, m, cx| draft_saved(m, cx), IMAP_DRAFT_EDIT),
    // S7-04, шаг 8: «Все папки» находит письмо старше первых 500.
    scene("st-s704-far", far_found, IMAP_FAR),
    // S7-05, шаг 1: рейл «стандартов»: почта, календарь, задачи, контакты; ни справочника, ни правил, ни автоответа.
    scene("st-s705-rail", dav_rail, &[Input::Until(mail_ready)]),
    // S7-05, шаг 2: неделя как в CalDAV, экземпляры серии.
    scene("st-s705-week", |_, m, cx| m.cal.read(cx).loaded && m.cal.read(cx).title_count(&titled(SERIES)) >= 2, DAV_WEEK),
    // S7-05, шаг 2: поиск по теме.
    more("st-s705-search", |_, m, cx| !m.cal.read(cx).results_ready(), DAV_SEARCH),
    // S7-05, шаг 3: встреча с user2 уходит, у user1 в неделе сразу.
    scene("st-s705-meeting", |_, m, cx| event_shown(m, cx, &titled(DAV_MEETING)), DAV_NEW_MEETING),
    // S7-05, шаг 4: user2 принял: у user1 «принял» (опрос до 2 мин).
    scene("st-s705-accepted", dav_answered, DAV_ANSWER),
    // S7-05, шаг 8: новая карточка: на сервере vCard (`peer.py`, CardDAV).
    scene("st-s705-contact", |_, m, cx| contact_there(m, cx), DAV_CONTACT_NEW),
    // S7-05, шаг 8: адрес карточки — в подсказках «Кому».
    scene("st-s705-suggest", |_, m, cx| screens::compose_tab(m).is_some_and(|c| c.read(cx).suggestions() > 0), DAV_SUGGEST),
    // S7-05, шаг 6: новая задача во «Входящих» задач: VTODO на сервере (`peer.py`, CalDAV).
    scene("st-s705-task", |_, m, cx| task(m, cx, FIRST_TASK).is_some(), DAV_TASK_NEW),
    // S7-05, шаг 6: срок.
    more("st-s705-task-due", |_, m, cx| task(m, cx, FIRST_TASK).is_some_and(|t| t.due.is_some()), DAV_TASK_DUE),
    // S7-05, шаг 6: правка в другом клиенте и новая задача — опросом, до 2 мин; строка состояния не держит «Загрузка».
    scene("st-s705-task-back", |s, m, cx| pushed_tasks(s, m, cx) && synced_line(m), TASKS_PUSHED),
    // S7-05, шаг 6: выполнение: `STATUS:COMPLETED` на сервере.
    scene("st-s705-task-done", |_, m, cx| task(m, cx, EDITED_TASK).is_some_and(|t| t.progress.is_closed()), DAV_TASK_DONE),
    // S7-05, шаг 6: удаление: задачи на сервере нет.
    scene("st-s705-task-gone", |_, m, cx| task(m, cx, SERVER_TASK).is_none(), DELETE_TASK),
    // S7-05, шаг 7: поручение user2: письмо от user1, своей копии на сервере user2 нет.
    scene("st-s705-order", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Waiting), DAV_ORDER),
    // S7-05, шаг 7: окно «Отменить» прошло: выход из прогона не забирает письмо, которое держит синхронизация.
    more("st-s705-order-sent", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Waiting), PAST_UNDO),
    // S7-05, шаг 7: у user2 задача пришла письмом iMIP.
    scene("st-s705-order-got", |_, m, cx| status_of(m, cx, ORDER) == Some(Status::Accepted), TAKE_ORDER),
    // S7-05, шаг 5: коллега не открыл календарь: «нет данных», а не «свободен».
    scene("st-s705-nobusy", |_, m, cx| lane(m, cx) == Some(None), ASSISTANT),
    // S7-05, шаг 5: открыл: занятость видна.
    scene("st-s705-busy", |_, m, cx| lane(m, cx).flatten().is_some_and(|n| n > 0), ASSISTANT),
    // S7-04, шаг 3: письмо с HTML и вложением: текст, список вложений, внешняя картинка не загружена.
    scene("st-s704-html", html_open, IMAP_HTML),
    // S7-04, шаг 3: просмотр вложения.
    more("st-s704-preview", |_, m, cx| screens::reading(m, cx).is_some_and(|r| r.preview.is_some()), IMAP_PREVIEW),
    // S7-04, шаг 9: «Отменить»: письмо не ушло, черновик открыт снова.
    scene("st-s704-undo", |_, m, cx| !m.undo_shown() && undo_draft(m, cx), IMAP_UNDO_SEND),
    // S7-04, шаг 9: без «Отменить»: письмо ушло (`peer.py`: у user2).
    more("st-s704-resend", |_, m, cx| !undo_draft(m, cx) && !m.undo_shown(), IMAP_RESEND),
    // S7-04, шаг 9: письмо в окне «Отменить», Postman закрывается: письмо уходит при закрытии (`peer.py`: у user2).
    scene("st-s704-quit", |_, m, _| screens::compose_tab(m).is_none() && m.undo_shown(), IMAP_QUIT),
    more("st-s704-quit-close", |_, _, _| false, IMAP_CLOSE),
    // S7-04, шаг 10: ответ — в одной беседе с письмом.
    scene("st-s704-thread", thread_of_two, IMAP_THREAD),
    // S7-04, шаг 10: фильтр «Все папки» находит письмо за первыми 500.
    scene("st-s704-filter", filter_found, IMAP_FILTER),
    // S8-43, шаг 1: «Открыть коллеге»: в списке с уровнем «Только занятость».
    scene("st-s843-share", |_, m, cx| access_of(m, cx) == Some(false), SHARE),
    // S8-43, шаг 1: у коллеги в помощнике занятость владельца.
    scene("st-s843-busy", |_, m, cx| lane(m, cx).flatten().is_some_and(|n| n > 0), ASSISTANT),
    // S8-43, шаг 2: щелчок по уровню: «Занятость и подробности».
    scene("st-s843-level", |_, m, cx| access_of(m, cx) == Some(true), SHARE_LEVEL),
    // S8-43, шаг 2: у коллеги — событие владельца.
    scene("st-s843-seen", |_, m, cx| event_shown(m, cx, &titled(BUSY_EVENT)), TO_WEEK),
    // S8-43, шаг 3: открыть ещё раз: запись заменилась, повтора нет.
    scene("st-s843-again", |_, m, cx| access_of(m, cx) == Some(false) && access_count(m, cx) == 1, SHARE_AGAIN),
    // S8-43, шаг 5: новый запуск: список с сервера.
    scene("st-s843-kept", |_, m, cx| access_of(m, cx) == Some(false), SHARE_KEPT),
    // S8-43, шаг 4: «Закрыть доступ».
    more("st-s843-close", |_, m, cx| access_of(m, cx).is_none(), SHARE_CLOSE),
    // S8-43, шаг 4: у коллеги снова «нет данных».
    scene("st-s843-none", |_, m, cx| lane(m, cx) == Some(None), ASSISTANT),
    // S6-05, шаг 4: Ctrl+N в «Сегодня»: задача со сроком сегодня.
    scene("st-s605-today", |_, m, cx| card_shown(m, cx, CARD_TASK), CARD_NEW),
    // S6-05, шаг 3: Space — зачёркнута, повторный Space — отмена.
    more("st-s605-space", |_, m, cx| task(m, cx, CARD_TASK).is_some_and(|t| !t.progress.is_closed()), CARD_SPACE),
    // S6-05, шаг 3: выполнена: тост, на сервере `STATUS:COMPLETED`.
    more("st-s605-closed", |_, m, cx| task(m, cx, CARD_TASK).is_some_and(|t| t.progress.is_closed()), CARD_CLOSE),
    // S6-05, шаг 3: «Отменить» — снова в работе, и на сервере.
    more("st-s605-undone", |_, m, cx| task(m, cx, CARD_TASK).is_some_and(|t| !t.progress.is_closed()), &[Input::Press("notice-act")]),
    // S6-05, шаг 5: Insert — срок «Завтра»; F2 — новое название.
    more("st-s605-keys", |_, m, cx| task(m, cx, CARD_RENAMED).is_some_and(|t| t.due.is_some()), CARD_KEYS),
    // S6-05, шаг 5: Del — удалена, на сервере нет.
    more("st-s605-del", |_, m, cx| task(m, cx, CARD_RENAMED).is_none(), CARD_DELETE),
    // S6-05, шаг 5: «Отменить» — снова на сервере.
    more("st-s605-undel", |_, m, cx| task(m, cx, CARD_RENAMED).is_some(), &[Input::Press("notice-act")]),
    // S6-06, шаг 1: напоминание, проект, категория: на сервере (`peer.py`, CalDAV).
    more("st-s606-fields", card_fields, CARD_FIELDS),
    // S6-06, шаг 2: подзадачи, отмеченная — сразу.
    more(
        "st-s606-subs",
        |_, m, cx| task(m, cx, CARD_RENAMED).is_some_and(|t| postman_core::tasks::subtask_progress(&m.tasks.read(cx).tasks, &t.uid) == (1, 2)),
        CARD_SUBS,
    ),
    // S6-06, шаг 3: заметки: переключить задачу и вернуться — текст сохранён.
    more("st-s606-notes", |_, m, cx| m.tasks.read(cx).notes.read(cx).value().as_str() == CARD_NOTE, CARD_NOTES),
    // S6-07, шаг 1: «Каждый месяц»: на сервере `RRULE`.
    more("st-s607-repeat", |_, m, cx| task(m, cx, CARD_RENAMED).and_then(postman_core::recur::rule_of).is_some(), CARD_REPEAT),
    // S6-07, шаг 2: выполнить: копия со следующим сроком, тост «следующая».
    more("st-s607-next", |_, m, cx| open_copies(m, cx) == 1 && toast_is(m, cx, DONE_NEXT), CARD_NEXT),
    // S6-07, шаг 2: «Отменить» — как было: одна задача с повтором.
    more("st-s607-undo", |_, m, cx| open_copies(m, cx) == 1 && closed_copies(m, cx) == 0, &[Input::Press("notice-act")]),
    // S6-08, шаг 1: блок «Время в календаре»: окно и кнопка.
    more("st-s608-card", |_, _, _| screens::spotted("block-place").is_some(), CARD_BLOCK),
    // S6-05, шаг 6: письмо со сроком — в «Из почты»; выполнить — срок снят; «Отменить» — вернулся.
    scene("st-s605-mail", |_, m, cx| letter_task(m, cx), FROM_MAIL),
    more("st-s605-mail-done", |_, m, cx| !letter_task(m, cx), MAIL_DONE),
    more("st-s605-mail-undo", |_, m, cx| letter_task(m, cx), &[Input::Press("notice-act")]),
    // S7-03а, шаг 1: полный сервер: на рейле все разделы.
    scene("st-s703a-rail", full_rail, &[Input::Until(mail_ready)]),
    // S7-03а, шаг 2: разделы настроек сервера на месте.
    more("st-s703a-settings", |_, m, cx| settings_on(m, Page::Calendar, cx), SETTINGS_PAGES),
    // S7-03а, шаг 3: календарь, контакты, задачи открываются, тоста «нет у сервера» нет.
    scene("st-s703a-sections", |s, m, cx| tasks_ready(s, m, cx) && !toast_is(m, cx, &tr!("feature-unavailable")), SECTIONS),
    // S7-03а, шаг 4: палитра: команды разделов сервера.
    scene("st-s703a-palette", palette_found, PALETTE_RULES),
    // S7-03а, шаг 5: «Контакты»: кнопки карточки на месте.
    scene(
        "st-s703a-contacts",
        |_, _, _| ["contact-write", "contact-meet", "contact-copy"].iter().all(|id| screens::spotted(id).is_some()),
        CARD_BUTTONS,
    ),
    // S7-03а, шаг 6: схема «Одиночные клавиши» применяется и возвращается.
    scene("st-s703a-keys", |_, _, cx| crate::prefs::get(cx).key_scheme == postman_core::prefs::KeyScheme::Outlook, KEY_SCHEME),
    // S5-21, шаг 8: поиск находит встречу на другой неделе; переход — неделя и подробности.
    scene("st-s521-found", |_, m, cx| m.cal.read(cx).results_ready(), FAR_SEARCH),
    more("st-s521-open", |_, m, cx| event_shown(m, cx, &titled(FAR_EVENT)), FAR_OPEN),
    // S5-22, шаг 4: календарь коллеги: его событие в неделе, общая встреча — один раз.
    scene("st-s522-week", colleague_week, TO_WEEK),
    // T1-06, шаги 1, 2: своя группа сворачивается: подпись выбрана, папки скрыты.
    scene("st-t106-fold", |_, m, cx| m.folders.read(cx).is_collapsed(""), OWN_FOLD),
    // T1-06, шаг 4: в календарь и обратно: группа осталась свёрнутой.
    more("st-t106-kept", |_, m, cx| m.section == crate::Section::Mail && m.folders.read(cx).is_collapsed(""), LEAVE_AND_BACK),
    // T1-06, шаг 3: ветка общего ящика: палитра открывает папку — ветка развернулась.
    more("st-t106-branch", branch_open, OPEN_BRANCH),
    // S7-07б, шаг 1: своя встреча: «Изменить» и «Отменить встречу».
    scene("st-s707-new", |_, m, cx| event_shown(m, cx, &titled(OWN_MEETING)), OWN_NEW),
    more("st-s707-buttons", |_, _, _| own_buttons(), OWN_SELECT),
    // S7-07б, шаг 1: у приглашённого кнопок нет.
    scene("st-s707-foreign", |_, m, cx| m.cal.read(cx).selected() && !own_buttons(), FOREIGN_SELECT),
    // S7-07б, шаг 2: новая тема и «Отправить»: тост, вкладка закрылась, в неделе — новое.
    scene("st-s707-edit", |_, m, cx| event_shown(m, cx, &titled(OWN_EDITED)) && toast_is(m, cx, UPDATED), OWN_EDIT),
    // S7-07б, шаг 3: «Не отменять» убирает строку, «Отменить встречу» — встречи нет.
    more("st-s707-cancel", |_, m, cx| m.section == crate::Section::Calendar && !m.cal.read(cx).has_title(&titled(OWN_EDITED)), OWN_CANCEL),
    // S8-23, шаг 1: книга Excel: листы разделами.
    scene("st-s823-book", |_, m, cx| sections_in(m, cx, 2), file_letter!(FILE_BOOK)),
    // S8-23, шаг 2: презентация: слайды разделами.
    scene("st-s823-slides", |_, m, cx| sections_in(m, cx, 2), file_letter!(FILE_SLIDES)),
    // S8-23, шаг 3: таблица CSV — сеткой.
    scene("st-s823-csv", |_, m, cx| sections_in(m, cx, 1), file_letter!(FILE_CSV)),
    // S8-23, шаг 4: письмо `.eml`: шапка, текст.
    scene("st-s823-eml", |_, m, cx| preview_is(m, cx, |s| matches!(s, crate::preview::Shown::Letter(_))), file_letter!(FILE_EML)),
    // S8-23, шаг 5: Markdown и RTF — текстом.
    scene("st-s823-md", |_, m, cx| preview_ready(m, cx), file_letter!(FILE_MD)),
    // T1-13в, шаг 1: документ Word.
    scene("st-t113-docx", |_, m, cx| sections_in(m, cx, 1), file_letter!(FILE_DOCX)),
    // T1-17, шаги 1, 3: три письма без тел в кэше: вкладка с тремя `.eml`; письмо уходит себе.
    scene("st-t117-many", |_, m, _| screens::compose_tab(m).is_none(), FORWARD_MANY),
    // T1-17, шаг 2: меню строки: одно вложение, тема «FW: …».
    scene(
        "st-t117-one",
        |_, m, cx| {
            screens::compose_tab(m).is_some_and(|c| c.read(cx).model.subject.starts_with("FW:") && c.read(cx).model.attachments().count() == 1)
        },
        FORWARD_ONE,
    ),
    // S5-12 Ф4, шаг 9: «Конфиденциально» без своего сертификата шифрования: «Нет сертификата: <свой адрес>».
    scene("st-s512-own", own_no_cert, OWN_CERT),
    // T1-02б, шаг 5: адрес не EWS: выбора «Войти как пользователь Windows» нет.
    scene("st-t102-plain", plain_login, PLAIN_LOGIN),
    // T1-23, шаги 1, 2: меню строки справочника: карточка на месте, «Копировать адрес», «Написать», «Встреча».
    scene("st-t123-menu", |_, m, _| m.tabs.active().kind == crate::tabs::TabKind::Meeting, CONTACT_MENU),
    // T1-23, шаг 3: «Мои контакты»: «Изменить» — форма контакта.
    scene("st-t123-mine", |_, m, cx| m.contacts.read(cx).editing(), MINE_MENU),
    // S6-11, шаги 1, 2: «Я поручил»: счётчик, группы; «Только свои».
    scene("st-s611-by-me", |_, m, cx| matches!(m.tasks.read(cx).nav, Nav::Delegations(Tab::ByMe)) && !m.tasks.read(cx).shown.is_empty(), DELEG_BY_ME),
    // S6-11, шаг 3: «Мне поручили»: у исполнителя задача в группе новых.
    scene("st-s611-to-me", |_, m, cx| matches!(m.tasks.read(cx).nav, Nav::Delegations(Tab::ToMe)) && task(m, cx, ORDER).is_some(), DELEG_TO_ME),
    // S6-11, шаг 6: Del на чужом поручении — ничего не удалено.
    more("st-s611-foreign", |_, m, cx| task(m, cx, ORDER).is_some() && m.toasts.read(cx).latest().is_some(), DELEG_FOREIGN),
    // S6-11, шаг 6: Del на своём поручении — «Поручение удалено», «Отменить» — на месте.
    scene("st-s611-delete", |_, m, cx| task(m, cx, ORDER).is_some(), DELEG_DELETE),
    // T1-10, шаг 1, T1-10в, шаги 1, 4: картинки `cid:` письма разобраны; ширина — по снимку.
    scene("st-t110-images", |_, m, cx| picked(m, cx, IMAGES).is_some() && screens::reading(m, cx).is_some_and(|r| r.images.len() >= 3), IMAGES_OPEN),
    // S8-16, шаг 1, и S8-15, шаг 5: перенос в «На этом компьютере» без вопроса.
    scene("st-s816-local", |_, m, cx| m.list.read(cx).selection.is_empty() && m.toasts.read(cx).latest().is_some(), TO_LOCAL),
    // S8-15, шаг 5: письмо открывается: тело — из базы.
    more("st-s815-letter", |_, m, cx| screens::reading(m, cx).is_some_and(|r| r.doc.is_some()), LOCAL_LETTER),
    // S8-15, шаг 4: «В папку ▾»: другие учётки и «На этом компьютере».
    scene("st-s815-menu", menu_has_groups, MOVE_MENU),
    // S8-15, шаг 3: Ctrl+A, перетащить на папку другой учётной записи: источник пуст.
    scene("st-s815-drag", |_, m, cx| m.list.read(cx).count == 0, DRAG_ALL),
    // S8-15, шаг 3: список цели — письма со значком «ждёт переноса»; S8-16, шаг 2: полоса вопроса.
    more("st-s815-target", |_, m, cx| ask_shown(m, cx) && m.list.read(cx).has_marks(), OPEN_TARGET),
    // S8-16, шаг 2: другая папка — полосы нет, вернуться — есть.
    more("st-s816-ask", |_, m, cx| ask_shown(m, cx), ASK_AWAY_AND_BACK),
    // S8-16, шаг 2: новый запуск: вопрос на месте.
    scene("st-s816-kept", |_, m, cx| ask_shown(m, cx), ASK_KEPT),
    // S8-16, шаг 3: «Перенести на сервер», строка состояния, «Пауза».
    more("st-s816-pause", |_, m, _| job_state(m) == Some(MigrationState::Paused), UPLOAD_PAUSE),
    // S8-16, шаг 3: «Продолжить».
    more("st-s816-resume", |_, m, _| job_state(m) == Some(MigrationState::Running), UPLOAD_RESUME),
    // S8-16, шаг 5: «Остановить» → «Не останавливать».
    more("st-s816-stop-no", |_, m, _| job_state(m) == Some(MigrationState::Running), STOP_NO),
    // S8-16, шаги 4, 7: новый запуск: перенос дошёл до конца.
    scene("st-s816-done", |_, m, _| no_jobs_left(m), RESUMED),
    // S8-16, шаг 5: новый перенос и «Вернуть на место».
    scene("st-s816-start", |_, m, _| job_state(m).is_some(), DRAG_AND_START),
    more("st-s816-return", |_, m, _| no_jobs_left(m), STOP_AND_RETURN),
    // S8-16, шаг 5: «Оставить остальные локально».
    scene("st-s816-start-2", |_, m, _| job_state(m).is_some(), DRAG_AND_START),
    more("st-s816-keep", |_, m, _| no_jobs_left(m), STOP_AND_KEEP),
    // S8-16, шаг 6: квота цели: полоса и «Перенос стоит: нет места».
    scene("st-s816-start-3", |_, m, _| job_state(m).is_some(), DRAG_AND_GO),
    more("st-s816-full", |_, m, _| job_state(m) == Some(MigrationState::QuotaFull), NO_SPACE),
    // S8-16, шаг 6: «Оставить локально».
    more("st-s816-local-rest", |_, m, _| no_jobs_left(m), KEEP_LOCAL),
];

fn start(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    Ok(())
}

static STAMP: LazyLock<String> = LazyLock::new(|| std::env::var("POSTMAN_STAND_STAMP").unwrap_or_default());

/// Письмо этого прогона: тема содержит текст и метку прогона.
fn mine(row: &EmailRow, text: &str) -> bool {
    row.subject.contains(text) && row.subject.contains(STAMP.as_str())
}

/// Строка списка с письмом этого прогона.
fn find(m: &Mailer, cx: &App, text: &str) -> Option<usize> {
    let (offset, rows) = &m.list.read(cx).rows;
    rows.iter().position(|r| mine(r, text)).map(|i| i + offset)
}

/// Точка строки с письмом этого прогона: только когда список уже нарисован.
fn placed(m: &Mailer, cx: &App, text: &str) -> Option<Point<Pixels>> {
    find(m, cx, text).and_then(|i| screens::row_place(m, cx, i))
}

/// Открытое письмо этого прогона.
fn picked<'a>(m: &'a Mailer, cx: &'a App, text: &str) -> Option<&'a EmailRow> {
    let list = m.list.read(cx);
    let id = screens::reading(m, cx).filter(|r| r.doc.is_some()).map(|r| r.row.id.as_str())?;
    list.rows.1.iter().find(|r| r.id == id && mine(r, text))
}

fn in_folder(m: &Mailer, cx: &App, name: &str) -> bool {
    let list = m.list.read(cx);
    m.folders.read(cx).mailboxes.iter().any(|b| b.name == name && list.mailbox.as_deref() == Some(b.id.as_str()))
}

fn open_folder(m: &mut Mailer, cx: &mut Context<Mailer>, name: &str) -> Result<(), String> {
    let id = m.folders.read(cx).mailboxes.iter().find(|b| b.name == name).map(|b| b.id.clone());
    m.open_mailbox(id.ok_or_else(|| format!("no folder {name}"))?, cx);
    Ok(())
}

fn mail_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.live.synced && rows_loaded(m.list.read(cx)) && !m.list.read(cx).computing
}

// --- правила ---

const TENDERS: &str = "Тендеры"; // i18n-ok: категория таблицы по умолчанию
const PROJECTS: &str = "Проекты"; // i18n-ok: папка стенда
const FIRST_TENDER: &str = "тендер проверка"; // i18n-ok: тема письма
const NEXT_TENDER: &str = "тендер снова"; // i18n-ok: тема письма
const ALPHA: &str = "Проект Альфа"; // i18n-ok: тема письма
const BETA: &str = "Проект Бета"; // i18n-ok: тема письма

fn rules<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a RulesPanel> {
    m.settings.rules_panel.as_ref().filter(|_| settings_on(m, Page::Rules, cx)).map(|p| p.read(cx))
}

/// Правил на сервере, когда панель не ждёт ответа и редактор закрыт.
fn rules_count(m: &Mailer, cx: &App) -> Option<usize> {
    rules(m, cx).filter(|p| !p.busy && p.editor.is_none()).and_then(|p| p.state.as_ref()).map(|s| s.rules.len())
}

fn first_rule_on(m: &Mailer, cx: &App) -> Option<bool> {
    rules(m, cx).filter(|p| !p.busy).and_then(|p| p.state.as_ref()).and_then(|s| s.rules.first()).map(|r| r.enabled)
}

fn open_rules(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.open_rules(window, cx);
    Ok(())
}

/// Ключевое слово категории «Тендеры»: таблицу держит экран почты, панель правил получает её копию.
fn tenders_keyword(m: &Mailer, cx: &App) -> Option<String> {
    m.own_categories(cx).list.iter().find(|c| c.name == TENDERS).map(|c| c.keyword.clone())
}

fn has_tenders(m: &Mailer, cx: &App, row: &EmailRow) -> bool {
    tenders_keyword(m, cx).is_some_and(|k| row.keywords.iter().any(|w| w.eq_ignore_ascii_case(&k)))
}

fn tenders_chip(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let i = m.settings.rules_panel.as_ref()?.read(cx).categories.iter().position(|(_, n, _)| n == TENDERS)?;
    screens::spotted(&format!("rules-category-{i}"))
}

fn projects_chip(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let i = m.settings.rules_panel.as_ref()?.read(cx).mailboxes.iter().position(|(_, n)| n == PROJECTS)?;
    screens::spotted(&format!("rules-folder-{i}"))
}

fn editor_kind(m: &Mailer, cx: &App, kind: Kind) -> bool {
    rules(m, cx).and_then(|p| p.editor.as_ref()).is_some_and(|d| d.kind == kind)
}

const RULE_CATEGORY: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_rules),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.state.is_some())),
    Input::Press("rules-add"),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.editor.is_some())),
    Input::Press("rules-value"),
    Input::Text("тендер"), // i18n-ok: ввод сценария
    Input::Press("rules-kind-5"),
    until!(|m, cx| editor_kind(m, cx, Kind::Category)),
    Input::Press("rules-editor-save"),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.error.as_deref() == Some(tr!("rules-bad-category").as_str()))),
    Input::Click(tenders_chip, Held::No),
    until!(|m, cx| rules(m, cx).and_then(|p| p.editor.as_ref()).is_some_and(|d| d.category.is_some())),
    Input::Press("rules-editor-save"),
    until!(|m, cx| rules_count(m, cx) == Some(1)),
    until!(|m, cx| rules(m, cx).and_then(|p| p.state.as_ref()).is_some_and(|s| matches!(s.rules[0].action, Action::Category(_)))),
];

const LABEL: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, FIRST_TENDER).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, FIRST_TENDER), Held::No),
];

const DELETE_RULE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_rules),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.state.as_ref().is_some_and(|s| !s.rules.is_empty()))),
    Input::Press("rule-delete-0"),
    Input::Press("rule-delete-yes-0"),
    until!(|m, cx| rules_count(m, cx) == Some(0)),
];

const PLAIN: &[Input] =
    &[Input::Until(mail_ready), until!(|m, cx| placed(m, cx, NEXT_TENDER).is_some()), Input::Click(|m, _, cx| placed(m, cx, NEXT_TENDER), Held::No)];

const RULE_MOVE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_rules),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.state.is_some())),
    Input::Press("rules-add"),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.editor.is_some())),
    Input::Press("rules-value"),
    Input::Text("проект"), // i18n-ok: ввод сценария
    until!(|m, cx| editor_kind(m, cx, Kind::Move)),
    Input::Click(projects_chip, Held::No),
    until!(|m, cx| rules(m, cx).and_then(|p| p.editor.as_ref()).is_some_and(|d| d.folder.is_some())),
    Input::Press("rules-editor-save"),
    until!(|m, cx| rules_count(m, cx) == Some(1)),
];

const MOVED: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| open_folder(m, cx, PROJECTS)),
    until!(|m, cx| in_folder(m, cx, PROJECTS) && find(m, cx, ALPHA).is_some()),
];

const RULE_OFF: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_rules),
    until!(|m, cx| first_rule_on(m, cx) == Some(true)),
    Input::Press("rule-on-0"),
    until!(|m, cx| first_rule_on(m, cx) == Some(false)),
];

const BACK_IN_INBOX: &[Input] = &[Input::Until(mail_ready), until!(|m, cx| find(m, cx, BETA).is_some())];

const BAD_ADDRESS: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_rules),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.state.is_some())),
    Input::Press("rules-add"),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.editor.is_some())),
    Input::Press("rules-value"),
    Input::Text("счёт"), // i18n-ok: ввод сценария
    Input::Press("rules-kind-4"),
    until!(|m, cx| editor_kind(m, cx, Kind::Redirect)),
    Input::Press("rules-address"),
    Input::Text("без-собаки"), // i18n-ok: ввод сценария
    Input::Press("rules-editor-save"),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.error.as_deref() == Some(tr!("rules-bad-address").as_str()))),
    Input::Press("rules-editor-cancel"),
];

const FOREIGN: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_rules),
    until!(|m, cx| rules(m, cx).and_then(|p| p.state.as_ref()).is_some_and(|s| !s.foreign.is_empty())),
    Input::Press("rules-add"),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.editor.is_some())),
    Input::Press("rules-value"),
    Input::Text("чужой"), // i18n-ok: ввод сценария
    Input::Press("rules-kind-1"),
    until!(|m, cx| editor_kind(m, cx, Kind::MarkRead)),
    Input::Press("rules-editor-save"),
    until!(|m, cx| rules(m, cx).is_some_and(|p| p.conflict.is_some())),
    Input::Press("rules-replace-no"),
];

// --- автоответ ---

const BOLD_WORD: &str = "**слово**"; // i18n-ok: ввод сценария
const WORD: &str = "слово"; // i18n-ok: ввод сценария
const INSIDE: &str = "Внутри компании"; // i18n-ok: ввод сценария
const OUTSIDE: &str = "Вне компании"; // i18n-ok: ввод сценария
const INSIDE_TEXT: &str = "Коллегам: я в отпуске."; // i18n-ok: ввод сценария
const OUTSIDE_TEXT: &str = "Внешним: я в отпуске."; // i18n-ok: ввод сценария

fn vacation<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a VacationPanel> {
    m.settings.vacation_panel.as_ref().filter(|_| settings_on(m, Page::Vacation, cx)).map(|p| p.read(cx)).filter(|p| !p.loading)
}

fn open_vacation(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.open_vacation(window, cx);
    Ok(())
}

const FORMAT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_vacation),
    until!(|m, cx| vacation(m, cx).is_some()),
    Input::Press("vacation-body"),
    Input::Text(WORD),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.form.reply.internal.body == WORD)),
    Input::Keys(&["ctrl-a"]),
    Input::Press("vac-bold"),
];

/// Панель автоответа длиннее окна: прокрутка над правым столбцом, где нет поля текста со своей прокруткой.
fn beside_form(_: &Mailer, window: &Window, _: &App) -> Option<Point<Pixels>> {
    let view = window.viewport_size();
    Some(point(view.width - px(120.), view.height - px(150.)))
}

const VACATION_DOWN: Input = Input::Wheel(beside_form, 2000.);
const VACATION_UP: Input = Input::Wheel(beside_form, -2000.);

const CANCEL: &[Input] = &[VACATION_DOWN, Input::Press("vacation-cancel"), VACATION_UP];

const EXTERNAL: &[Input] = &[
    Input::Press("vacation-tab-1"),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.tab_index() == 1)),
    // Флажок стоит по умолчанию; на сервере без наших настроек — снят.
    Input::Run(|_, m, _, cx| {
        if let Some(p) = &m.settings.vacation_panel {
            p.update(cx, |p, cx| {
                p.form.reply.contacts_only = true;
                cx.notify();
            });
        }
        Ok(())
    }),
    Input::Press("vacation-contacts-only"),
];

/// Дата через `offset` дней, как её вводят в полях периода.
fn day(offset: i64) -> String {
    let today = jiff::Zoned::now();
    today.checked_add(jiff::Span::new().days(offset)).unwrap_or(today).strftime("%d.%m.%Y").to_string()
}

const BAD_ORDER: &[Input] = &[
    Input::Press("vacation-enabled"),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.form.enabled)),
    Input::Press("vacation-period"),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.form.period)),
    Input::Press("vacation-from"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Typed(|| day(2)),
    Input::Press("vacation-to"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Typed(|| day(0)),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.form.to == day(0) && p.form.from == day(2))),
    VACATION_DOWN,
    Input::Press("vacation-save"),
];

const SAVE_VACATION: &[Input] = &[
    VACATION_UP,
    Input::Press("vacation-from"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Typed(|| day(0)),
    Input::Press("vacation-to"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Typed(|| day(2)),
    // Без этого шага сцена могла сохранить «с» прошлой сцены, и автоответ ждал бы начала периода.
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.form.from == day(0) && p.form.to == day(2))),
    Input::Press("vacation-tab-0"),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.tab_index() == 0)),
    Input::Press("vacation-subject"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(INSIDE),
    Input::Press("vacation-body"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(INSIDE_TEXT),
    Input::Press("vacation-tab-1"),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.tab_index() == 1)),
    Input::Press("vacation-subject"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(OUTSIDE),
    Input::Press("vacation-body"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text(OUTSIDE_TEXT),
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.form.reply.external.body == OUTSIDE_TEXT && p.form.reply.internal.subject == INSIDE)),
    VACATION_DOWN,
    // «Вне офиса» включено по умолчанию.
    until!(|m, cx| vacation(m, cx).is_some_and(|p| p.form.reply.out_of_office)),
    Input::Press("vacation-save"),
];

/// Сохранено; на Stalwart панель говорит, что применён один текст всем (`STALWART.md` п. 14а).
fn vacation_saved(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.settings.bar.is_some() && vacation(m, cx).is_some_and(|p| !p.busy && !p.dirty() && p.form.enabled && p.limit.is_some() && p.error.is_none())
}

fn event_shown(m: &Mailer, cx: &App, title: &str) -> bool {
    m.section == crate::Section::Calendar && m.cal.read(cx).loaded && m.cal.read(cx).has_title(title)
}

const KEPT: &[Input] = &[Input::Until(mail_ready), until!(|m, _| m.settings.bar.is_some()), Input::Run(open_vacation)];

/// Stalwart хранит один текст автоответа: закладка «Внешним» и флажки после перезапуска — как на сервере, а не как сохраняли.
fn kept(_: &Scene, m: &Mailer, cx: &App) -> bool {
    vacation(m, cx).is_some_and(|p| p.form.enabled && p.form.period && !p.form.reply.internal.subject.is_empty())
}

const TURN_OFF_UNDO: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, _| m.settings.bar.is_some()),
    Input::Press("vacation-bar-off"),
    until!(|m, cx| m.settings.bar.is_none() && m.toasts.read(cx).latest() == Some(tr!("vacation-turned-off").as_str())),
    Input::Press("notice-act"),
];

const TURN_OFF: &[Input] = &[Input::Until(mail_ready), until!(|m, _| m.settings.bar.is_some()), Input::Press("vacation-bar-off")];

fn quota_toast(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.settings.quota.is_some_and(|u| u.percent() > 90) && m.toasts.read(cx).on_screen() >= 1
}

const QUOTA: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, _| m.settings.quota.is_some_and(|u| u.percent() > 90)),
    until!(|m, cx| m.toasts.read(cx).on_screen() >= 1),
    Input::Run(|_, m, _, cx| {
        m.toasts.update(cx, |t, cx| t.hide_all(cx));
        Ok(())
    }),
    until!(|m, cx| m.toasts.read(cx).on_screen() == 0),
    Input::Press("quota"),
];

const CLEAN: &[Input] = &[Input::Press("notice-act")];

// --- задачи ---

const FIRST_TASK: &str = "Задача стенда"; // i18n-ok: ввод сценария
const EDITED_TASK: &str = "Задача правка"; // i18n-ok: тема `peer.py`
const SERVER_TASK: &str = "С сервера"; // i18n-ok: тема `peer.py`
const ORDER: &str = "Поручение стенда"; // i18n-ok: ввод сценария
const SECOND_ORDER: &str = "Поручение второе"; // i18n-ok: ввод сценария

/// Название задачи этого прогона.
fn titled(word: &str) -> String {
    format!("{word} {}", *STAMP)
}

fn task<'a>(m: &'a Mailer, cx: &'a App, word: &str) -> Option<&'a Task> {
    let title = titled(word);
    m.tasks.read(cx).tasks.iter().find(|t| t.title == title)
}

fn status_of(m: &Mailer, cx: &App, word: &str) -> Option<Status> {
    task(m, cx, word).and_then(postman_calendar::assign::status)
}

fn tasks_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.live.synced && m.section == crate::Section::Tasks && m.tasks.read(cx).ready()
}

/// Выбрать задачу в списке `nav`.
fn pick(m: &mut Mailer, cx: &mut Context<Mailer>, nav: Nav, word: &str) -> Result<(), String> {
    let title = titled(word);
    m.tasks.update(cx, |t, cx| {
        t.set_nav(nav, cx);
        t.select_title(&title, cx);
    });
    if m.tasks.read(cx).selected_task().is_some_and(|t| t.title == title) { Ok(()) } else { Err(format!("no task {title}")) }
}

fn today() -> Nav {
    Nav::List(List::Today)
}

/// Раздел — командой: после смены сцены фокуса в окне нет, Ctrl+4 не дойдёт.
const OPEN_TASKS: Input = Input::Run(|_, m, _, cx| {
    m.set_section(crate::Section::Tasks, cx);
    Ok(())
});

const NEW_TASK: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Keys(&["ctrl-n"]),
    Input::Typed(|| titled(FIRST_TASK)),
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, FIRST_TASK).is_some()),
];

fn pushed_tasks(_: &Scene, m: &Mailer, cx: &App) -> bool {
    task(m, cx, EDITED_TASK).is_some() && task(m, cx, SERVER_TASK).is_some() && task(m, cx, FIRST_TASK).is_none()
}

const TASKS_PUSHED: &[Input] = &[OPEN_TASKS, Input::Until(tasks_ready), Input::Until(pushed_tasks)];

const IMPORTANT: &[Input] =
    &[OPEN_TASKS, Input::Until(tasks_ready), Input::Run(|_, m, _, cx| pick(m, cx, today(), EDITED_TASK)), Input::Press("card-prio")];

const SECOND_WINDOW: &[Input] = &[OPEN_TASKS, Input::Until(tasks_ready)];

const PLACE_BLOCK: &[Input] =
    &[OPEN_TASKS, Input::Until(tasks_ready), Input::Run(|_, m, _, cx| pick(m, cx, today(), EDITED_TASK)), Input::Press("block-place")];

const CALENDAR_KEY: &[Input] = &[Input::Run(to_calendar)];

const REMOVE_BLOCK: &[Input] =
    &[OPEN_TASKS, Input::Until(tasks_ready), Input::Run(|_, m, _, cx| pick(m, cx, today(), EDITED_TASK)), Input::Press("block-remove")];

const COMPLETE: &[Input] =
    &[OPEN_TASKS, Input::Until(tasks_ready), Input::Run(|_, m, _, cx| pick(m, cx, today(), EDITED_TASK)), Input::Press("card-check")];

const DELETE_TASK: &[Input] = &[
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Inbox), SERVER_TASK)),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.delete(cx));
        Ok(())
    }),
];

// --- поручения ---

/// Адрес из окружения прогона: в репозитории адресов стенда нет.
fn colleague(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn delegate(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.tasks.update(cx, |t, cx| t.delegate(window, cx));
    Ok(())
}

const ASSIGN: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Keys(&["ctrl-n"]),
    Input::Typed(|| titled(ORDER)),
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, ORDER).is_some()),
    Input::Run(delegate),
    Input::Typed(|| colleague("POSTMAN_STAND_PEER")),
    Input::Keys(&["enter"]),
    until!(|m, cx| m.tasks.read(cx).screen_form_open()),
    Input::Press("assign-watcher-field"),
    Input::Typed(|| colleague("POSTMAN_STAND_WATCHER")),
    Input::Keys(&["enter"]),
    Input::Press("assign-due-1"),
    Input::Press("assign-submit"),
    until!(|m, cx| status_of(m, cx, ORDER).is_some()),
];

const ACCEPT_RESULT: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    until!(|m, cx| status_of(m, cx, ORDER) == Some(Status::Done)),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::Delegations(Tab::ByMe), ORDER)),
    Input::Press("assign-accept-result"),
];

const TAKE_ORDER: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    until!(|m, cx| status_of(m, cx, ORDER) == Some(Status::Waiting)),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::Delegations(Tab::ToMe), ORDER)),
    Input::Press("assign-accept"),
];

const WORK_ORDER: &[Input] =
    &[Input::Press("assign-start"), until!(|m, cx| status_of(m, cx, ORDER) == Some(Status::InProgress)), Input::Press("assign-finish")];

const ORDER_CLOSED: &[Input] = &[until!(|m, cx| status_of(m, cx, ORDER) == Some(Status::Closed))];

fn toast_is(m: &Mailer, cx: &App, text: &str) -> bool {
    m.toasts.read(cx).latest().is_some_and(|t| t.starts_with(text))
}

const REMIND_ALL: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_nav(today(), cx));
        Ok(())
    }),
    Input::Keys(&["ctrl-n"]),
    Input::Typed(|| titled(SECOND_ORDER)),
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, SECOND_ORDER).is_some()),
    Input::Run(delegate),
    Input::Typed(|| colleague("POSTMAN_STAND_PEER")),
    Input::Keys(&["enter"]),
    until!(|m, cx| m.tasks.read(cx).screen_form_open()),
    Input::Press("assign-submit"),
    until!(|m, cx| status_of(m, cx, SECOND_ORDER) == Some(Status::Waiting)),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_tab(Tab::ByMe, cx));
        Ok(())
    }),
    Input::Press("deleg-remind-all"),
    until!(|m, cx| toast_is(m, cx, REMINDED)),
    Input::Run(|_, m, _, cx| {
        m.toasts.update(cx, |t, cx| t.hide_all(cx));
        Ok(())
    }),
    Input::Press("deleg-remind-all"),
];

const REMINDED: &str = "Напомнили"; // i18n-ok: начало тоста
const REMINDED_ALREADY: &str = "Всем уже напомнили"; // i18n-ok: начало тоста

const REVOKE_UNDO: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::Delegations(Tab::ByMe), SECOND_ORDER)),
    Input::Press("assign-revoke"),
    until!(|m, cx| task(m, cx, SECOND_ORDER).is_some_and(|t| postman_calendar::assign::status(t).is_none())),
    Input::Press("notice-act"),
];

/// `peer.py` 14 с сверяет, что письма отмены нет: новая отмена — после этого.
const REVOKE: &[Input] = &[
    Input::Run(|_, _, _, _| {
        *lock_clock() = Some(Instant::now());
        Ok(())
    }),
    until!(|_, _| lock_clock().is_some_and(|t| t.elapsed() > Duration::from_secs(16))),
    Input::Press("assign-revoke"),
];

static CLOCK: Mutex<Option<Instant>> = Mutex::new(None);

fn lock_clock() -> std::sync::MutexGuard<'static, Option<Instant>> {
    CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn no_closed_in_wait(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let waiting = m.tasks.read(cx).waiting_keywords();
    m.section == crate::Section::Mail && task(m, cx, ORDER).is_some_and(|t| !waiting.contains(&postman_calendar::assign::keyword(&t.uid)))
}

const OPEN_WAIT: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_tab(Tab::ByMe, cx));
        Ok(())
    }),
    Input::Press("deleg-open-wait"),
];

// --- общие ящики ---

const TO_SHARED: &str = "Общему ящику"; // i18n-ok: тема `peer.py`
const TO_READONLY: &str = "Только чтение"; // i18n-ok: тема `peer.py`

/// «Входящие» общего ящика: имя ящика содержит `name`.
fn shared_inbox(m: &Mailer, cx: &App, name: &str) -> Option<String> {
    let f = m.folders.read(cx);
    let account = f.accounts.iter().find(|a| a.name.contains(name))?;
    f.of(Some(&account.id)).find(|b| b.role.as_deref() == Some("inbox")).map(|b| b.id.clone())
}

fn open_shared(m: &mut Mailer, cx: &mut Context<Mailer>, name: &str) -> Result<(), String> {
    let id = shared_inbox(m, cx, name).ok_or_else(|| format!("no shared mailbox {name}"))?;
    m.open_mailbox(id, cx);
    Ok(())
}

fn taken_row<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a EmailRow> {
    let (_, rows) = &m.list.read(cx).rows;
    rows.iter().find(|r| mine(r, TO_SHARED))
}

fn taken_by(m: &Mailer, cx: &App, who: &str) -> bool {
    taken_row(m, cx).and_then(crate::shared::taken).is_some_and(|t| t.by(who))
}

const SHARED_TREE: &[Input] = &[Input::Until(mail_ready)];

fn both_shared(_: &Scene, m: &Mailer, cx: &App) -> bool {
    shared_inbox(m, cx, "shared-sales").is_some() && shared_inbox(m, cx, "shared-ro").is_some()
}

const SHARED_PUSH: &[Input] = &[
    Input::Until(mail_ready),
    Input::Until(both_shared),
    Input::Run(|_, m, _, cx| open_shared(m, cx, "shared-sales")),
    until!(|m, cx| placed(m, cx, TO_SHARED).is_some()),
];

const TAKE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Until(both_shared),
    Input::Run(|_, m, _, cx| open_shared(m, cx, "shared-sales")),
    until!(|m, cx| placed(m, cx, TO_SHARED).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_SHARED), Held::No),
    until!(|m, cx| picked(m, cx, TO_SHARED).is_some()),
    Input::Keys(&["ctrl-j"]),
];

const SEES_TAKEN: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| shared_inbox(m, cx, "shared-sales").is_some()),
    Input::Run(|_, m, _, cx| open_shared(m, cx, "shared-sales")),
    until!(|m, cx| placed(m, cx, TO_SHARED).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_SHARED), Held::No),
    until!(|m, cx| taken_by(m, cx, &colleague("POSTMAN_STAND_PEER"))),
];

const STEAL: &[Input] = &[Input::Keys(&["ctrl-j"])];

const LOST: &[Input] = &[until!(|m, cx| taken_by(m, cx, &colleague("POSTMAN_STAND_PEER")))];

const READ_ONLY: &[Input] = &[
    Input::Until(mail_ready),
    Input::Until(both_shared),
    Input::Run(|_, m, _, cx| open_shared(m, cx, "shared-ro")),
    until!(|m, cx| placed(m, cx, TO_READONLY).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_READONLY), Held::No),
    until!(|m, cx| picked(m, cx, TO_READONLY).is_some()),
    Input::Keys(&["delete"]),
];

const REPLY_SHARED: &[Input] = &[
    Input::Until(mail_ready),
    Input::Until(both_shared),
    Input::Run(|_, m, _, cx| open_shared(m, cx, "shared-sales")),
    until!(|m, cx| placed(m, cx, TO_SHARED).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_SHARED), Held::No),
    until!(|m, cx| picked(m, cx, TO_SHARED).is_some()),
    Input::Keys(&["ctrl-r"]),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.from().email.contains("shared-sales"))),
    Input::Text(ANSWER),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.body.contains(ANSWER))),
    Input::Keys(&["ctrl-enter"]),
    until!(|m, _| m.compose.inline.is_none()),
];

const ANSWER: &str = "Ответ от ящика"; // i18n-ok: ввод сценария

const FLAG_AND_DELETE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Until(both_shared),
    Input::Run(|_, m, _, cx| open_shared(m, cx, "shared-sales")),
    until!(|m, cx| placed(m, cx, TO_SHARED).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_SHARED), Held::No),
    until!(|m, cx| picked(m, cx, TO_SHARED).is_some()),
    Input::Run(|_, m, _, cx| {
        m.toggle_flag(cx);
        Ok(())
    }),
    until!(|m, cx| picked(m, cx, TO_SHARED).is_some_and(|r| r.flagged)),
];

const DELETE_SHARED: &[Input] = &[
    Input::Run(|_, m, _, cx| {
        m.delete_selected(cx);
        Ok(())
    }),
    until!(|m, cx| find(m, cx, TO_SHARED).is_none()),
];

// --- несколько учётных записей ---

const TO_SECOND: &str = "Второй учётке"; // i18n-ok: тема `peer.py`
const FROM_SECOND: &str = "От второй учётки"; // i18n-ok: ввод сценария
const DELETE_SECOND: &str = "Удалить у второй"; // i18n-ok: тема `peer.py`

/// «Настройки → Аккаунты» без карточки прошлой сцены.
fn accounts_page(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.cancel_add_account(cx);
    m.open_settings(Some(Page::Accounts), window, cx);
    Ok(())
}

fn add_panel(m: &Mailer, cx: &App) -> Option<(bool, bool)> {
    m.settings.add.as_ref().map(|p| (p.read(cx).busy, p.read(cx).error.is_some()))
}

fn add_error<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a str> {
    m.settings.add.as_ref().and_then(|p| p.read(cx).error.as_deref())
}

fn extra_count(m: &Mailer) -> usize {
    m.live.profile.names.len()
}

const fn sign_in_steps(password: fn() -> String) -> [Input; 11] {
    [
        Input::Until(mail_ready),
        Input::Run(accounts_page),
        Input::Press("settings-account-add"),
        until!(|m, cx| add_panel(m, cx).is_some()),
        Input::Typed(|| colleague("POSTMAN_URL")),
        Input::Keys(&["enter"]),
        Input::Typed(|| colleague("POSTMAN_STAND_PEER")),
        Input::Keys(&["enter"]),
        // Шаг «Пароль»: поле в фокусе. «Не хранить»: в диспетчер учётных данных ничего не пишется.
        Input::Typed(password),
        Input::Press("signin-store-never"),
        Input::Press("signin-next"),
    ]
}

const ADD_WRONG: [Input; 11] = sign_in_steps(|| "wrong-password".to_owned());
const ADD_RIGHT: [Input; 11] = sign_in_steps(|| colleague("POSTMAN_STAND_PEER_SECRET"));

const SAME_ACCOUNT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(accounts_page),
    Input::Press("settings-account-add"),
    until!(|m, cx| add_panel(m, cx).is_some()),
    Input::Typed(|| colleague("POSTMAN_URL")),
    Input::Keys(&["enter"]),
    Input::Typed(|| colleague("POSTMAN_USER")),
    Input::Keys(&["enter"]),
    Input::Text("any"),
    Input::Press("signin-store-never"),
    Input::Press("signin-next"),
    until!(|m, cx| add_panel(m, cx).is_some_and(|(busy, error)| !busy && error)),
];

fn second_account(m: &Mailer, cx: &App) -> Option<String> {
    let f = m.folders.read(cx);
    f.accounts.iter().find(|a| postman_cache::profile_of(Some(&a.id)) == Some(a.id.as_str())).map(|a| a.id.clone())
}

fn second_folder(m: &Mailer, cx: &App, name: &str) -> Option<String> {
    let f = m.folders.read(cx);
    let account = second_account(m, cx)?;
    f.of(Some(&account)).find(|b| b.role.as_deref() == Some(name) || b.name == name).map(|b| b.id.clone())
}

fn open_second(m: &mut Mailer, cx: &mut Context<Mailer>, name: &str) -> Result<(), String> {
    let id = second_folder(m, cx, name).ok_or_else(|| format!("no folder {name} in the second account"))?;
    m.open_mailbox(id, cx);
    Ok(())
}

const SECOND_LETTER: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| second_folder(m, cx, "inbox").is_some()),
    Input::Run(|_, m, _, cx| open_second(m, cx, "inbox")),
    until!(|m, cx| placed(m, cx, TO_SECOND).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_SECOND), Held::No),
    until!(|m, cx| picked(m, cx, TO_SECOND).is_some_and(|r| r.seen)),
];

const SECOND_REPLY: &[Input] = &[
    Input::Keys(&["ctrl-r"]),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.from().email == colleague("POSTMAN_STAND_PEER"))),
    Input::Keys(&["escape"]),
];

const SECOND_MOVE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| open_second(m, cx, "inbox")),
    until!(|m, cx| placed(m, cx, TO_SECOND).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_SECOND), Held::No),
    until!(|m, cx| picked(m, cx, TO_SECOND).is_some()),
    Input::Keys(&["ctrl-shift-v"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(crate::menu::Sub::Move))),
    Input::Keys(&["enter"]),
    until!(|m, cx| find(m, cx, TO_SECOND).is_none()),
];

const SECOND_DELETE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| open_second(m, cx, "inbox")),
    until!(|m, cx| placed(m, cx, DELETE_SECOND).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, DELETE_SECOND), Held::No),
    until!(|m, cx| picked(m, cx, DELETE_SECOND).is_some()),
    Input::Run(|_, m, _, cx| {
        m.delete_selected(cx);
        Ok(())
    }),
    until!(|m, cx| find(m, cx, DELETE_SECOND).is_none()),
];

const SEND_FROM_SECOND: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-n"]),
    until!(|m, _| screens::compose_tab(m).is_some()),
    Input::Typed(|| format!("{};", colleague("POSTMAN_USER"))),
    Input::Keys(&["tab"]),
    Input::Typed(|| titled(FROM_SECOND)),
    Input::Keys(&["tab"]),
    Input::Text("Письмо от второй учётной записи."), // i18n-ok: ввод сценария
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| !c.read(cx).model.body.is_empty())),
    Input::Press("from"),
    until!(|_, _| screens::spot_first("from-item").is_some()),
    Input::Click(second_sender, Held::No),
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| c.read(cx).model.from().email == colleague("POSTMAN_STAND_PEER"))),
    Input::Press("send"),
    until!(|m, _| screens::compose_tab(m).is_none()),
];

fn second_sender(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let c = screens::compose_tab(m)?.read(cx);
    let peer = colleague("POSTMAN_STAND_PEER");
    let i = c.senders.iter().position(|s| s.address.email == peer)?;
    screens::spotted(&format!("from-item-{i}"))
}

const SEARCH_BOTH: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, window, cx| {
        m.search.update(cx, |s, cx| s.focus(window, cx));
        Ok(())
    }),
    Input::Run(|_, m, _, cx| {
        m.bar.update(cx, |b, _| b.scope = crate::bar::Scope::All);
        Ok(())
    }),
    Input::Typed(|| STAMP.clone()),
];

fn found_both(m: &Mailer, cx: &App) -> bool {
    let ids = m.list.read(cx).search_ids.as_ref();
    ids.is_some_and(|ids| {
        let own = ids.iter().any(|i| postman_cache::account_of(i).is_none());
        let other = ids.iter().any(|i| postman_cache::account_of(i).is_some());
        own && other
    })
}

const SECOND_EVENT: &str = "Событие второй"; // i18n-ok: тема `peer.py`
const BOTH_MEETING: &str = "Встреча обеих"; // i18n-ok: ввод сценария

const SECOND_CALENDAR: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Run(|_, m, _, cx| {
        m.cal.update(cx, |c, cx| c.today(cx));
        Ok(())
    }),
];

fn second_meeting_sender(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let view = m.meetings.last()?;
    let peer = colleague("POSTMAN_STAND_PEER");
    let i = view.read(cx).sender_position(&peer)?;
    screens::spotted(&format!("meeting-from-item-{i}"))
}

const MEETING_FROM_SECOND: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_meeting),
    until!(|m, _| m.meetings.last().is_some()),
    Input::Press("meeting-title"),
    Input::Typed(|| titled(BOTH_MEETING)),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Typed(|| format!("{};", colleague("POSTMAN_USER"))),
    Input::Press("meeting-from"),
    until!(|_, _| screens::spot_first("meeting-from-item").is_some()),
    Input::Click(second_meeting_sender, Held::No),
    until!(|m, cx| m.meetings.last().is_some_and(|v| v.read(cx).model.from.as_deref() == Some(colleague("POSTMAN_STAND_PEER").as_str()))),
    Input::Keys(&["ctrl-enter"]),
    until!(|m, _| m.meetings.last().is_none()),
    Input::Run(to_calendar),
];

const REMOVE_SECOND: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(accounts_page),
    Input::Press("settings-extra-remove-0"),
    Input::Press("settings-extra-remove-yes-0"),
    until!(|m, _| extra_count(m) == 0),
];

fn second_gone(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let peer = colleague("POSTMAN_STAND_PEER");
    extra_count(m) == 0 && second_account(m, cx).is_none() && !m.compose.senders().iter().any(|s| s.address.email == peer)
}

// --- живой ящик: разделы 14 и 34 ---

const WEEK_EVENT: &str = "Встреча стенда"; // i18n-ok: тема `peer.py`

fn unread_in_inbox(m: &Mailer, cx: &App) -> Option<u32> {
    let list = m.list.read(cx);
    m.folders.read(cx).mailboxes.iter().find(|b| list.mailbox.as_deref() == Some(b.id.as_str())).map(|b| b.unread)
}

const UNREAD_FELL: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, UNREAD_LETTER).is_some()),
    Input::Run(|_, m, _, cx| {
        let before = unread_in_inbox(m, cx).ok_or("no folder row")?;
        UNREAD_BEFORE.store(before, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }),
    Input::Click(|m, _, cx| placed(m, cx, UNREAD_LETTER), Held::No),
    until!(|m, cx| picked(m, cx, UNREAD_LETTER).is_some_and(|r| r.seen)),
    until!(|m, cx| unread_in_inbox(m, cx) == Some(UNREAD_BEFORE.load(std::sync::atomic::Ordering::Relaxed).saturating_sub(1))),
];

const UNREAD_ROSE: &[Input] = &[
    Input::Run(|_, m, _, cx| {
        m.mark(Some(false), cx);
        Ok(())
    }),
    until!(|m, cx| unread_in_inbox(m, cx) == Some(UNREAD_BEFORE.load(std::sync::atomic::Ordering::Relaxed))),
];

static UNREAD_BEFORE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const UNREAD_LETTER: &str = "Непрочитанное"; // i18n-ok: тема `peer.py`

const WEEK_ADDED: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Run(|_, m, _, cx| {
        m.cal.update(cx, |c, cx| c.today(cx));
        Ok(())
    }),
];

const WEEK_DELETED: &[Input] = &[Input::Keys(&["ctrl-2"])];

fn typed_directory(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::compose_tab(m).is_some_and(|c| c.read(cx).suggestion_names().iter().any(|t| t.contains("Test user")))
}

const DIRECTORY_NAMES: &[Input] =
    &[Input::Until(mail_ready), Input::Keys(&["ctrl-n"]), until!(|m, _| screens::compose_tab(m).is_some()), Input::Text("test")];

/// Календарь после закрытой вкладки встречи: фокуса в окне нет, клавиша не дойдёт.
fn to_calendar(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.set_section(crate::Section::Calendar, cx);
    Ok(())
}

/// Вкладка встречи — командой, не клавишей: Ctrl+Shift+Q не доходит, пока фокус в поле письма прошлой сцены.
fn open_meeting(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.open_meeting(None, cx);
    Ok(())
}

fn meeting_chip(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.meetings.last().is_some_and(|v| v.read(cx).chip_names().iter().any(|t| t.contains("Test user")))
}

const MEETING_NAMES: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_meeting),
    until!(|m, _| m.meetings.last().is_some()),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Text("test"),
    until!(|m, cx| m.meetings.last().is_some_and(|v| v.read(cx).suggestions().0 > 0)),
    Input::Keys(&["enter"]),
];

// --- IMAP и SMTP (S7-04), CalDAV и CardDAV (S7-05) ---

const WITH_FILE: &str = "С вложением"; // i18n-ok: тема `peer.py`
const TO_MYSELF: &str = "Себе с вложением"; // i18n-ok: ввод сценария
const IMAP_DRAFT: &str = "Черновик IMAP"; // i18n-ok: ввод сценария
const SERIES: &str = "Серия стенда"; // i18n-ok: тема `peer.py`
const DAV_MEETING: &str = "Встреча CalDAV"; // i18n-ok: ввод сценария
const DAV_CONTACT: &str = "Контакт стенда"; // i18n-ok: ввод сценария

fn imap_tree(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let f = m.folders.read(cx);
    let has = |role: &str| f.mailboxes.iter().any(|b| b.role.as_deref() == Some(role));
    let features = crate::features::get(cx);
    use postman_core::model::Feature as F;
    m.live.synced
        && ["inbox", "drafts", "sent", "junk", "trash"].iter().all(|r| has(r))
        && !features.has(F::Calendar)
        && !features.has(F::Rules)
        && !features.has(F::Autoreply)
}

const IMAP_FILE: &[Input] =
    &[Input::Until(mail_ready), until!(|m, cx| placed(m, cx, WITH_FILE).is_some()), Input::Click(|m, _, cx| placed(m, cx, WITH_FILE), Held::No)];

fn open_with_file(m: &mut Mailer, cx: &mut Context<Mailer>, subject: String) {
    let mut draft = postman_compose::Draft::default();
    draft.attachments.push(crate::composer::scenes::sample_attachment());
    draft.to = vec![postman_compose::Address::new("", colleague("POSTMAN_USER"))];
    draft.subject = subject;
    m.open_compose(draft, cx);
}

const IMAP_SEND: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        open_with_file(m, cx, titled(TO_MYSELF));
        Ok(())
    }),
    until!(|m, _| screens::compose_tab(m).is_some()),
    Input::Press("send"),
    until!(|m, _| screens::compose_tab(m).is_none()),
    Input::Run(|_, m, _, cx| {
        m.set_section(crate::Section::Mail, cx);
        Ok(())
    }),
    until!(|m, cx| find(m, cx, TO_MYSELF).is_some()),
];

const IMAP_FLAGS: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, TO_MYSELF).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, TO_MYSELF), Held::No),
    until!(|m, cx| picked(m, cx, TO_MYSELF).is_some_and(|r| r.seen)),
    Input::Run(|_, m, _, cx| {
        m.toggle_flag(cx);
        Ok(())
    }),
    until!(|m, cx| picked(m, cx, TO_MYSELF).is_some_and(|r| r.flagged)),
    Input::Keys(&["1"]),
    until!(|m, cx| picked(m, cx, TO_MYSELF).is_some_and(|r| r.keywords.iter().any(|k| k.starts_with("pm.cat.")))),
];

const IMAP_FLAG_BACK: &[Input] = &[until!(|m, cx| picked(m, cx, TO_MYSELF).is_some_and(|r| !r.flagged))];

const IMAP_MOVE: &[Input] = &[
    Input::Keys(&["ctrl-shift-v"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(crate::menu::Sub::Move))),
    Input::Keys(&["enter"]),
    until!(|m, cx| find(m, cx, TO_MYSELF).is_none()),
];

/// Письмо `WITH_FILE`: «Удалить» — в «Удалённые».
const IMAP_DELETE: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, WITH_FILE).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, WITH_FILE), Held::No),
    until!(|m, cx| picked(m, cx, WITH_FILE).is_some()),
    Input::Run(|_, m, _, cx| {
        m.delete_selected(cx);
        Ok(())
    }),
    until!(|m, cx| find(m, cx, WITH_FILE).is_none()),
];

fn trash_of(m: &Mailer, cx: &App) -> Option<String> {
    m.folders
        .read(cx)
        .mailboxes
        .iter()
        .find(|b| postman_cache::account_of(&b.id).is_none() && b.role.as_deref() == Some("trash"))
        .map(|b| b.id.clone())
}

const IMAP_PURGE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        let id = trash_of(m, cx).ok_or("no trash")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| placed(m, cx, WITH_FILE).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, WITH_FILE), Held::No),
    until!(|m, cx| picked(m, cx, WITH_FILE).is_some()),
    Input::Run(|_, m, _, cx| {
        m.delete_selected(cx);
        Ok(())
    }),
    until!(|m, cx| m.list.read(cx).confirm.is_some()),
    Input::Press("purge-yes"),
    until!(|m, cx| find(m, cx, WITH_FILE).is_none()),
];

fn draft_saved(m: &Mailer, cx: &App) -> bool {
    screens::compose_tab(m).is_some_and(|c| matches!(c.read(cx).model.status, crate::compose::Status::Saved { .. }))
}

const IMAP_DRAFT_EDIT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-n"]),
    until!(|m, _| screens::compose_tab(m).is_some()),
    Input::Keys(&["alt-s", "tab"]),
    Input::Typed(|| titled(IMAP_DRAFT)),
    Input::Keys(&["tab"]),
    Input::Text("первая версия"), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-s"]),
    Input::Until(|_, m, cx| draft_saved(m, cx)),
    Input::Text(" и вторая"), // i18n-ok: ввод сценария
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| c.read(cx).model.body.contains("вторая"))), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-s"]),
    Input::Until(|_, m, cx| draft_saved(m, cx)),
];

const IMAP_FAR: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-e"]),
    Input::Run(|_, m, _, cx| {
        m.bar.update(cx, |b, _| b.scope = crate::bar::Scope::All);
        Ok(())
    }),
    Input::Typed(|| colleague("POSTMAN_STAND_WORD")),
];

fn far_found(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).search_ids.as_ref().is_some_and(|ids| !ids.is_empty())
}

fn dav_rail(_: &Scene, m: &Mailer, cx: &App) -> bool {
    use postman_core::model::Feature as F;
    let features = crate::features::get(cx);
    m.live.synced
        && features.has(F::Calendar)
        && features.has(F::Tasks)
        && features.has(F::Contacts)
        && !features.has(F::Directory)
        && !features.has(F::Rules)
        && !features.has(F::Autoreply)
}

const DAV_WEEK: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-2"]),
    Input::Keys(&["3"]),
    Input::Run(|_, m, _, cx| {
        m.cal.update(cx, |c, cx| c.today(cx));
        Ok(())
    }),
];

const DAV_SEARCH: &[Input] =
    &[Input::Keys(&["ctrl-e"]), Input::Typed(|| titled(SERIES)), until!(|m, cx| m.cal.read(cx).results_ready()), Input::Keys(&["escape"])];

fn meeting_view(m: &Mailer) -> Option<Entity<crate::meeting::MeetingView>> {
    m.meetings.last()
}

const DAV_NEW_MEETING: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_meeting),
    until!(|m, _| meeting_view(m).is_some()),
    Input::Press("meeting-title"),
    Input::Typed(|| titled(DAV_MEETING)),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Typed(|| format!("{};", colleague("POSTMAN_STAND_PEER"))),
    Input::Keys(&["ctrl-enter"]),
    until!(|m, _| meeting_view(m).is_none()),
    Input::Run(to_calendar),
];

fn dav_answered(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.section == crate::Section::Calendar && m.cal.read(cx).attendee_accepted(&titled(DAV_MEETING))
}

/// Опрос папок молчит: «Загрузка: …» — только первая загрузка, дальше строка держит «Синхронизировано».
fn synced_line(m: &Mailer) -> bool {
    m.live.status == tr!("sync-synced")
}

const DAV_ANSWER: &[Input] = &[Input::Until(mail_ready), Input::Run(to_calendar)];

const DAV_CONTACT_NEW: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        m.set_section(crate::Section::Contacts, cx);
        Ok(())
    }),
    until!(|m, _| m.section == crate::Section::Contacts),
    Input::Run(|_, m, window, cx| {
        m.contacts.update(cx, |c, cx| c.new_contact(window, cx));
        Ok(())
    }),
    until!(|m, cx| m.contacts.read(cx).editing()),
    Input::Typed(|| titled(DAV_CONTACT)),
    Input::Press("contacts-emails"),
    Input::Typed(|| format!("{}@example.test", *STAMP)),
    Input::Keys(&["enter"]),
    until!(|m, cx| !m.contacts.read(cx).editing()),
];

fn contact_there(m: &Mailer, cx: &App) -> bool {
    let mail = format!("{}@example.test", *STAMP);
    m.contacts.read(cx).personal_emails().contains(&mail)
}

const DAV_TASK_NEW: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_nav(Nav::List(List::Inbox), cx));
        Ok(())
    }),
    Input::Keys(&["ctrl-n"]),
    Input::Typed(|| titled(FIRST_TASK)),
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, FIRST_TASK).is_some()),
];

const DAV_TASK_DUE: &[Input] = &[
    // Тост прошлой сцены лежит поверх строки «Срок» (находка F3): щелчок ушёл бы в него.
    Input::Run(|_, m, _, cx| {
        m.toasts.update(cx, |t, cx| t.hide_all(cx));
        Ok(())
    }),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Inbox), FIRST_TASK)),
    Input::Press("card-due"),
    Input::Press("due-opt-1"),
];

const DAV_TASK_DONE: &[Input] = &[
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Upcoming), EDITED_TASK)),
    Input::Press("card-check"),
];

const DAV_ORDER: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_nav(today(), cx));
        Ok(())
    }),
    Input::Keys(&["ctrl-n"]),
    Input::Typed(|| titled(ORDER)),
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, ORDER).is_some()),
    Input::Run(delegate),
    Input::Typed(|| colleague("POSTMAN_STAND_PEER")),
    Input::Keys(&["enter"]),
    until!(|m, cx| m.tasks.read(cx).screen_form_open()),
    Input::Press("assign-submit"),
];

/// Окно «Отменить» — 10 с.
const PAST_UNDO: &[Input] = &[
    Input::Run(|_, _, _, _| {
        *lock_clock() = Some(Instant::now());
        Ok(())
    }),
    until!(|_, _| lock_clock().is_some_and(|t| t.elapsed() > Duration::from_secs(14))),
];

const DAV_SUGGEST: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        m.open_compose(postman_compose::Draft::default(), cx);
        Ok(())
    }),
    until!(|m, _| screens::compose_tab(m).is_some()),
    Input::Typed(|| STAMP.clone()),
];

// --- S7-05, шаг 5, и S8-43: занятость и доступ к календарю ---

const BUSY_EVENT: &str = "Занятость стенда"; // i18n-ok: тема `peer.py`

/// Помощник планирования: занятость коллеги.
fn lane(m: &Mailer, cx: &App) -> Option<Option<usize>> {
    meeting_view(m)?.read(cx).lane_of(&colleague("POSTMAN_STAND_PEER"))
}

const ASSISTANT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_meeting),
    until!(|m, _| meeting_view(m).is_some()),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Typed(|| format!("{};", colleague("POSTMAN_STAND_PEER"))),
    until!(|m, cx| lane(m, cx).is_some()),
];

fn access(m: &Mailer, cx: &App) -> Vec<postman_core::sync::Access> {
    m.cal.read(cx).access_list().to_vec()
}

fn access_count(m: &Mailer, cx: &App) -> usize {
    let peer = colleague("POSTMAN_STAND_PEER");
    access(m, cx).iter().filter(|a| a.email == peer).count()
}

/// Уровень доступа коллеги: `false` — только занятость.
fn access_of(m: &Mailer, cx: &App) -> Option<bool> {
    let peer = colleague("POSTMAN_STAND_PEER");
    access(m, cx).iter().find(|a| a.email == peer).map(|a| a.items)
}

/// Строка коллеги: на стенде в списке могут быть и другие записи.
fn access_spot(m: &Mailer, cx: &App, id: &str) -> Option<Point<Pixels>> {
    let peer = colleague("POSTMAN_STAND_PEER");
    let n = access(m, cx).iter().position(|a| a.email == peer)?;
    screens::spotted(&format!("{id}-{n}"))
}

/// Левая панель календаря: строки доступа — внизу, за краем окна.
fn calendar_side(_: &Mailer, window: &Window, _: &App) -> Option<Point<Pixels>> {
    Some(point(px(150.), window.viewport_size().height / 2.))
}

const SIDE_DOWN: Input = Input::Wheel(calendar_side, 3000.);

const TO_WEEK: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(to_calendar),
    Input::Run(|_, m, _, cx| {
        m.cal.update(cx, |c, cx| c.today(cx));
        Ok(())
    }),
];

const SHARE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(to_calendar),
    until!(|m, cx| m.section == crate::Section::Calendar && m.cal.read(cx).loaded),
    SIDE_DOWN,
    Input::Press("share-open"),
    // Строка открытия растёт вниз, за край окна.
    SIDE_DOWN,
    Input::Typed(|| colleague("POSTMAN_STAND_PEER")),
    Input::Press("share"),
    until!(|m, cx| access_of(m, cx).is_some()),
];

const SHARE_LEVEL: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(to_calendar),
    until!(|m, cx| access_of(m, cx) == Some(false)),
    SIDE_DOWN,
    Input::Click(|m, _, cx| access_spot(m, cx, "cal-access-level"), Held::No),
];

const SHARE_AGAIN: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(to_calendar),
    until!(|m, cx| access_of(m, cx) == Some(true)),
    SIDE_DOWN,
    Input::Press("share-open"),
    SIDE_DOWN,
    Input::Typed(|| colleague("POSTMAN_STAND_PEER")),
    Input::Press("share"),
];

const SHARE_KEPT: &[Input] = &[Input::Until(mail_ready), Input::Run(to_calendar), until!(|m, cx| access_of(m, cx).is_some())];

const SHARE_CLOSE: &[Input] = &[SIDE_DOWN, Input::Click(|m, _, cx| access_spot(m, cx, "cal-access-close"), Held::No)];

// --- S7-04, шаги 3, 9, 10 ---

const RICH: &str = "Письмо с оформлением"; // i18n-ok: тема `peer.py`
const RICH_FILE: &str = "отчёт.txt"; // i18n-ok: вложение `peer.py`
const IMAP_UNDO: &str = "Отмена IMAP"; // i18n-ok: ввод сценария
const IMAP_CLOSED: &str = "Закрытие IMAP"; // i18n-ok: ввод сценария
const THREAD_LETTER: &str = "Беседа IMAP"; // i18n-ok: тема `peer.py`

fn html_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx)
        .filter(|_| picked(m, cx, RICH).is_some())
        .is_some_and(|r| r.body.as_ref().is_some_and(|b| b.attachments.iter().any(|a| a.name == RICH_FILE)))
}

const IMAP_HTML: &[Input] =
    &[Input::Until(mail_ready), until!(|m, cx| placed(m, cx, RICH).is_some()), Input::Click(|m, _, cx| placed(m, cx, RICH), Held::No)];

const IMAP_PREVIEW: &[Input] = &[Input::Press("att-card-0")];

fn undo_draft(m: &Mailer, cx: &App) -> bool {
    screens::compose_tab(m).is_some_and(|c| c.read(cx).model.subject == titled(IMAP_UNDO))
}

const IMAP_UNDO_SEND: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        let draft = postman_compose::Draft {
            to: vec![postman_compose::Address::new("", colleague("POSTMAN_STAND_PEER"))],
            subject: titled(IMAP_UNDO),
            markdown: "Письмо отменяется.".into(), // i18n-ok: ввод сценария
            ..Default::default()
        };
        m.open_compose(draft, cx);
        Ok(())
    }),
    until!(|m, _| screens::compose_tab(m).is_some()),
    Input::Press("send"),
    until!(|m, _| m.undo_shown()),
    Input::Press("undo-send"),
    until!(|m, cx| undo_draft(m, cx)),
];

const IMAP_QUIT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        let draft = postman_compose::Draft {
            to: vec![postman_compose::Address::new("", colleague("POSTMAN_STAND_PEER"))],
            subject: titled(IMAP_CLOSED),
            markdown: "Письмо уходит при закрытии.".into(), // i18n-ok: ввод сценария
            ..Default::default()
        };
        m.open_compose(draft, cx);
        Ok(())
    }),
    until!(|m, _| screens::compose_tab(m).is_some()),
    Input::Press("send"),
    until!(|m, _| screens::compose_tab(m).is_none() && m.undo_shown()),
];

/// Как «✕» заголовка: окно закрывается, процесс выходит сам (мимо `finish` прогона).
const IMAP_CLOSE: &[Input] = &[Input::Run(|_, _, window, _| {
    window.remove_window();
    Ok(())
})];

/// `peer.py` 12 с сверяет, что отменённого письма нет: отправка — после этого.
const IMAP_RESEND: &[Input] = &[
    Input::Run(|_, _, _, _| {
        *lock_clock() = Some(Instant::now());
        Ok(())
    }),
    until!(|_, _| lock_clock().is_some_and(|t| t.elapsed() > Duration::from_secs(14))),
    Input::Press("send"),
    until!(|m, _| m.undo_shown()),
];

/// Письмо `THREAD_LETTER` и его ответ — одна беседа: лента в области чтения из двух писем.
fn thread_of_two(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.reading.read(cx).feed.as_ref().is_some_and(|f| f.conv.messages.len() >= 2)
}

const IMAP_THREAD: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, THREAD_LETTER).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, THREAD_LETTER), Held::No),
    until!(|m, cx| picked(m, cx, THREAD_LETTER).is_some()),
    Input::Keys(&["ctrl-r"]),
    until!(|m, _| m.compose.inline_view().is_some()),
    Input::Text(ANSWER),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.body.contains(ANSWER))),
    Input::Keys(&["ctrl-enter"]),
    until!(|m, _| m.compose.inline.is_none()),
    // Окно «Отменить» прошло, копия в «Отправленных» пришла в кэш; лента собирается при открытии письма.
    until!(|m, cx| thread_letters(m, cx) >= 2),
    Input::Click(|m, _, cx| placed(m, cx, RICH), Held::No),
    until!(|m, cx| picked(m, cx, RICH).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, THREAD_LETTER), Held::No),
];

fn thread_letters(m: &Mailer, cx: &App) -> usize {
    let list = m.list.read(cx);
    list.rows.1.iter().find(|r| mine(r, THREAD_LETTER)).and_then(|r| m.cache.thread_len(&r.thread_id).ok()).unwrap_or(0)
}

fn filter_found(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let word = colleague("POSTMAN_STAND_WORD");
    let list = m.list.read(cx);
    !list.computing && list.rows.1.iter().any(|r| r.subject.contains(&word))
}

const IMAP_FILTER: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        m.filters.update(cx, |f, cx| {
            f.change(cx, |q| {
                q.scope = crate::filters::Where::All;
                q.criteria.toggle_check(postman_core::filters::Check::Unread);
            });
        });
        Ok(())
    }),
];

// --- перенос между учётками (S8-15, S8-16) ---

const LOCAL_FOLDER: &str = "Архив стенда"; // i18n-ok: ввод сценария

fn local_folder(m: &Mailer, cx: &App) -> Option<String> {
    m.folders.read(cx).mailboxes.iter().find(|b| crate::local::is_local(&b.id) && b.name == LOCAL_FOLDER).map(|b| b.id.clone())
}

fn place_of_folder(id: Option<String>) -> Option<Point<Pixels>> {
    screens::spotted(&format!("mb-{}", id?))
}

fn target_inbox(m: &Mailer, cx: &App) -> Option<String> {
    second_folder(m, cx, "inbox")
}

fn first_row(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    screens::row_place(m, cx, 0)
}

fn job_state(m: &Mailer) -> Option<MigrationState> {
    m.migration.active().map(|j| j.state)
}

fn no_jobs_left(m: &Mailer) -> bool {
    m.migration
        .jobs
        .values()
        .all(|j| !matches!(j.state, MigrationState::Running | MigrationState::Paused | MigrationState::QuotaFull | MigrationState::Asking))
}

fn inbox_open(m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    let f = m.folders.read(cx);
    f.mailboxes
        .iter()
        .any(|b| b.role.as_deref() == Some("inbox") && postman_cache::account_of(&b.id).is_none() && list.mailbox.as_deref() == Some(b.id.as_str()))
}

const TO_LOCAL: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        m.local_folder_edit(crate::local::FolderEdit::Create { parent: None, name: LOCAL_FOLDER.to_owned() }, cx);
        Ok(())
    }),
    until!(|m, cx| place_of_folder(local_folder(m, cx)).is_some()),
    Input::Click(first_row, Held::No),
    Input::Click(|m, _, cx| screens::row_place(m, cx, 2), Held::Shift),
    until!(|m, cx| m.list.read(cx).selection.len() == 3),
    Input::Drag(first_row, |m, _, cx| place_of_folder(local_folder(m, cx))),
    until!(|m, cx| m.toasts.read(cx).latest().is_some()),
];

const LOCAL_LETTER: &[Input] = &[
    Input::Run(|_, m, _, cx| {
        let id = local_folder(m, cx).ok_or("no local folder")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| rows_loaded(m.list.read(cx)) && m.list.read(cx).count >= 3),
    Input::Click(first_row, Held::No),
];

const MOVE_MENU: &[Input] = &[
    Input::Until(mail_ready),
    Input::Click(first_row, Held::No),
    until!(|m, cx| screens::reading(m, cx).is_some()),
    Input::Keys(&["ctrl-shift-v"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(crate::menu::Sub::Move))),
];

/// «В папку ▾»: своя учётка, затем другая учётка и «На этом компьютере» подписями групп.
fn menu_has_groups(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let targets = m.list.read(cx).folder_targets();
    let local = tr!("local-group");
    let has = |group: &str| targets.iter().any(|t| t.group.as_deref() == Some(group));
    m.menu.is_some() && has(&colleague("POSTMAN_STAND_PEER")) && has(&local)
}

const DRAG_ALL: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| inbox_open(m, cx) && place_of_folder(target_inbox(m, cx)).is_some()),
    Input::Click(first_row, Held::No),
    Input::Keys(&["ctrl-a"]),
    until!(|m, cx| m.list.read(cx).selection.len() > 1),
    Input::Drag(first_row, |m, _, cx| place_of_folder(target_inbox(m, cx))),
    until!(|m, cx| m.list.read(cx).count == 0),
];

const OPEN_TARGET: &[Input] = &[
    Input::Run(|_, m, _, cx| {
        let id = target_inbox(m, cx).ok_or("no target inbox")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| rows_loaded(m.list.read(cx)) && m.list.read(cx).has_marks()),
];

fn ask_shown(m: &Mailer, cx: &App) -> bool {
    let open = m.list.read(cx).mailbox.clone();
    matches!(m.migration.bar(open.as_deref()), Some(crate::migration::Bar::Ask(_)))
}

const ASK_AWAY_AND_BACK: &[Input] = &[
    Input::Run(|_, m, _, cx| {
        let id = second_folder(m, cx, "sent").ok_or("no sent folder")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| !ask_shown(m, cx)),
    Input::Run(|_, m, _, cx| {
        let id = target_inbox(m, cx).ok_or("no target inbox")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| ask_shown(m, cx)),
];

const ASK_KEPT: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| target_inbox(m, cx).is_some()),
    Input::Run(|_, m, _, cx| {
        let id = target_inbox(m, cx).ok_or("no target inbox")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| ask_shown(m, cx)),
];

const UPLOAD_PAUSE: &[Input] = &[
    Input::Press("migration-to-server"),
    until!(|m, _| job_state(m) == Some(MigrationState::Running)),
    Input::Press("migration-status"),
    until!(|m, _| m.migration.open),
    Input::Press("migration-pause"),
    until!(|m, _| job_state(m) == Some(MigrationState::Paused)),
];

const UPLOAD_RESUME: &[Input] = &[Input::Press("migration-resume"), until!(|m, _| job_state(m) == Some(MigrationState::Running))];

const STOP_NO: &[Input] = &[
    Input::Press("migration-stop"),
    until!(|m, _| m.migration.stopping.is_some()),
    Input::Press("migration-stop-no"),
    until!(|m, _| m.migration.stopping.is_none()),
];

const RESUMED: &[Input] = &[Input::Until(mail_ready), until!(|m, _| no_jobs_left(m))];

const DRAG_AND_START: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| inbox_open(m, cx) && place_of_folder(target_inbox(m, cx)).is_some()),
    Input::Click(first_row, Held::No),
    Input::Keys(&["ctrl-a"]),
    until!(|m, cx| m.list.read(cx).selection.len() > 1),
    Input::Drag(first_row, |m, _, cx| place_of_folder(target_inbox(m, cx))),
    until!(|m, cx| m.list.read(cx).count == 0),
    Input::Run(|_, m, _, cx| {
        let id = target_inbox(m, cx).ok_or("no target inbox")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| ask_shown(m, cx)),
    Input::Press("migration-to-server"),
    until!(|m, _| job_state(m).is_some()),
    // Пауза: сотни писем уходят быстрее, чем сцена дойдёт до «Остановить».
    Input::Press("migration-status"),
    until!(|m, _| m.migration.open),
    Input::Press("migration-pause"),
    until!(|m, _| job_state(m) == Some(MigrationState::Paused)),
];

/// То же без паузы: цель без места останавливает задание сама.
const DRAG_AND_GO: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| inbox_open(m, cx) && place_of_folder(target_inbox(m, cx)).is_some()),
    Input::Click(first_row, Held::No),
    Input::Keys(&["ctrl-a"]),
    until!(|m, cx| m.list.read(cx).selection.len() > 1),
    Input::Drag(first_row, |m, _, cx| place_of_folder(target_inbox(m, cx))),
    until!(|m, cx| m.list.read(cx).count == 0),
    Input::Run(|_, m, _, cx| {
        let id = target_inbox(m, cx).ok_or("no target inbox")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
    until!(|m, cx| ask_shown(m, cx)),
    Input::Press("migration-to-server"),
    until!(|m, _| job_state(m).is_some()),
];

const STOP_AND_RETURN: &[Input] = &[
    Input::Press("migration-stop"),
    until!(|m, _| m.migration.stopping.is_some()),
    Input::Press("migration-stop-return"),
    until!(|m, _| no_jobs_left(m)),
];

const STOP_AND_KEEP: &[Input] = &[
    Input::Press("migration-stop"),
    until!(|m, _| m.migration.stopping.is_some()),
    Input::Press("migration-stop-keep"),
    until!(|m, _| no_jobs_left(m)),
];

const NO_SPACE: &[Input] = &[until!(|m, _| job_state(m) == Some(MigrationState::QuotaFull))];

const KEEP_LOCAL: &[Input] = &[Input::Press("migration-keep-local-short"), until!(|m, _| no_jobs_left(m))];

// --- задачи на живом ящике: S6-05, шаги 3–6, S6-06, шаги 1–3, S6-07, шаги 1, 2, S6-08, шаг 1 ---

const CARD_TASK: &str = "Карточка стенда"; // i18n-ok: ввод сценария
const CARD_RENAMED: &str = "Карточка новая"; // i18n-ok: ввод сценария
const CARD_PROJECT: &str = "Проект стенда"; // i18n-ok: ввод сценария
const CARD_NOTE: &str = "Заметка **жирным**"; // i18n-ok: ввод сценария
const DONE_NEXT: &str = "Выполнено · следующая"; // i18n-ok: начало тоста
const MAIL_DUE: &str = "Письмо со сроком"; // i18n-ok: тема `peer.py`

/// Клавиши — экрану: поле прошлой сцены могло уйти с экрана.
const FOCUS: Input = Input::Run(|_, m, window, cx| {
    window.focus(&m.focus, cx);
    Ok(())
});

fn card_shown(m: &Mailer, cx: &App, word: &str) -> bool {
    let title = titled(word);
    m.tasks.read(cx).shown.iter().any(|t| t.title == title && t.due.is_some())
}

const CARD_NEW: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_nav(today(), cx));
        Ok(())
    }),
    FOCUS,
    Input::Keys(&["ctrl-n"]),
    until!(|m, cx| m.tasks.read(cx).draft.is_some()),
    Input::Typed(|| titled(CARD_TASK)),
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, CARD_TASK).is_some()),
    Input::Run(|_, m, _, cx| pick(m, cx, today(), CARD_TASK)),
];

fn pending(m: &Mailer, cx: &App, word: &str) -> bool {
    task(m, cx, word).is_some_and(|t| m.tasks.read(cx).completing.is_pending(&t.uid))
}

const CARD_SPACE: &[Input] =
    &[FOCUS, Input::Keys(&["space"]), until!(|m, cx| pending(m, cx, CARD_TASK)), Input::Keys(&["space"]), until!(|m, cx| !pending(m, cx, CARD_TASK))];

const CARD_CLOSE: &[Input] = &[FOCUS, Input::Keys(&["space"]), until!(|m, cx| m.toasts.read(cx).latest().is_some())];

const CARD_KEYS: &[Input] = &[
    until!(|m, cx| m.tasks.read(cx).shown.iter().any(|t| t.title == titled(CARD_TASK))),
    Input::Run(|_, m, _, cx| pick(m, cx, today(), CARD_TASK)),
    FOCUS,
    Input::Keys(&["insert"]),
    until!(|m, cx| m.tasks.read(cx).due_open),
    Input::Press("due-opt-1"),
    until!(|m, cx| !m.tasks.read(cx).due_open),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Upcoming), CARD_TASK)),
    FOCUS,
    Input::Keys(&["f2"]),
    until!(|m, cx| m.tasks.read(cx).title.is_some()),
    Input::Keys(&["ctrl-a"]),
    Input::Typed(|| titled(CARD_RENAMED)),
    Input::Keys(&["enter"]),
];

const CARD_DELETE: &[Input] = &[Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Upcoming), CARD_RENAMED)), FOCUS, Input::Keys(&["delete"])];

fn card_fields(_: &Scene, m: &Mailer, cx: &App) -> bool {
    task(m, cx, CARD_RENAMED).is_some_and(|t| {
        !t.alerts.is_empty() && t.project.as_deref() == Some(titled(CARD_PROJECT).as_str()) && t.keywords.iter().any(|k| k.starts_with("pm.cat."))
    })
}

const CARD_FIELDS: &[Input] = &[
    until!(|m, cx| task(m, cx, CARD_RENAMED).is_some()),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Upcoming), CARD_RENAMED)),
    Input::Press("card-remind"),
    until!(|m, cx| m.tasks.read(cx).open == Some(crate::tasks::Field::Remind)),
    Input::Press("remind-opt-1"),
    until!(|m, cx| m.tasks.read(cx).open.is_none()),
    Input::Press("card-project"),
    until!(|m, cx| m.tasks.read(cx).open == Some(crate::tasks::Field::Project)),
    Input::Press("proj-new"),
    Input::Typed(|| titled(CARD_PROJECT)),
    Input::Keys(&["enter"]),
    until!(|m, cx| m.tasks.read(cx).open.is_none()),
    Input::Press("card-category"),
    until!(|m, cx| m.tasks.read(cx).open == Some(crate::tasks::Field::Category)),
    Input::Press("cat-opt-1"),
];

const CARD_SUBS: &[Input] = &[
    Input::Press("sub-new"),
    Input::Text("Первая"), // i18n-ok: ввод сценария
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, CARD_RENAMED).is_some_and(|t| postman_core::tasks::subtask_progress(&m.tasks.read(cx).tasks, &t.uid).1 == 1)),
    Input::Text("Вторая"), // i18n-ok: ввод сценария
    Input::Keys(&["enter"]),
    until!(|m, cx| task(m, cx, CARD_RENAMED).is_some_and(|t| postman_core::tasks::subtask_progress(&m.tasks.read(cx).tasks, &t.uid).1 == 2)),
    Input::Press("sub-1"),
];

const CARD_NOTES: &[Input] = &[
    until!(|_, _| screens::spotted("task-notes").is_some()),
    Input::Press("task-notes"),
    Input::Text(CARD_NOTE),
    until!(|m, cx| m.tasks.read(cx).notes.read(cx).value().contains("жирным")), // i18n-ok: ввод сценария
    FOCUS,
    Input::Keys(&["down"]),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Upcoming), CARD_RENAMED)),
];

fn open_copies(m: &Mailer, cx: &App) -> usize {
    let title = titled(CARD_RENAMED);
    m.tasks.read(cx).tasks.iter().filter(|t| t.title == title && !t.progress.is_closed()).count()
}

fn closed_copies(m: &Mailer, cx: &App) -> usize {
    let title = titled(CARD_RENAMED);
    m.tasks.read(cx).tasks.iter().filter(|t| t.title == title && t.progress.is_closed()).count()
}

const CARD_REPEAT: &[Input] = &[
    Input::Press("card-repeat"),
    until!(|m, cx| m.tasks.read(cx).open == Some(crate::tasks::Field::Repeat)),
    until!(|_, _| screens::spotted("repeat-opt-3").is_some()),
    Input::Press("repeat-opt-3"),
];

const CARD_NEXT: &[Input] = &[FOCUS, Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Upcoming), CARD_RENAMED)), Input::Keys(&["space"])];

const CARD_BLOCK: &[Input] = &[
    until!(|m, cx| task(m, cx, CARD_RENAMED).is_some()),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::List(List::Upcoming), CARD_RENAMED)),
    until!(|_, _| screens::spotted("block-place").is_some()),
];

fn letter_task(m: &Mailer, cx: &App) -> bool {
    m.section == crate::Section::Tasks && m.tasks.read(cx).shown.iter().any(|t| t.title.contains(MAIL_DUE) && t.title.contains(STAMP.as_str()))
}

const FROM_MAIL: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, MAIL_DUE).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, MAIL_DUE), Held::No),
    until!(|m, cx| picked(m, cx, MAIL_DUE).is_some()),
    Input::Keys(&["insert"]),
    Input::Press("lifecycle-pick-2"),
    until!(|m, _| m.lifecycle.picker.is_none()),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_nav(Nav::List(List::FromMail), cx));
        Ok(())
    }),
];

const MAIL_DONE: &[Input] = &[
    Input::Run(|_, m, _, cx| {
        let title = m.tasks.read(cx).shown.iter().find(|t| t.title.contains(MAIL_DUE)).map(|t| t.title.clone()).ok_or("no letter task")?;
        m.tasks.update(cx, |t, cx| t.select_title(&title, cx));
        Ok(())
    }),
    FOCUS,
    Input::Keys(&["space"]),
];

// --- S7-03а, S5-21, шаг 8, S5-22, шаг 4, T1-06: живой ящик ---

const FAR_EVENT: &str = "Встреча далеко"; // i18n-ok: тема `peer.py`
const COLLEAGUE_EVENT: &str = "Событие коллеги"; // i18n-ok: тема `peer.py`

fn full_rail(_: &Scene, m: &Mailer, cx: &App) -> bool {
    use postman_core::model::Feature as F;
    let features = crate::features::get(cx);
    m.live.synced && [F::Calendar, F::Tasks, F::Contacts, F::Directory, F::Rules, F::Autoreply].into_iter().all(|f| features.has(f))
}

const SETTINGS_PAGES: &[Input] = &[
    Input::Run(open_rules),
    until!(|m, cx| settings_on(m, Page::Rules, cx)),
    Input::Run(open_vacation),
    until!(|m, cx| settings_on(m, Page::Vacation, cx)),
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Calendar), window, cx);
        Ok(())
    }),
];

const SECTIONS: &[Input] = &[
    Input::Until(mail_ready),
    FOCUS,
    Input::Keys(&["ctrl-2"]),
    until!(|m, cx| m.section == crate::Section::Calendar && m.cal.read(cx).loaded),
    FOCUS,
    Input::Keys(&["ctrl-3"]),
    until!(|m, _| m.section == crate::Section::Contacts),
    FOCUS,
    Input::Keys(&["ctrl-4"]),
];

fn palette_found(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() > 0)
}

const PALETTE_RULES: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, window, cx| {
        m.toggle_palette(window, cx);
        Ok(())
    }),
    until!(|m, _| m.palette.is_some()),
    Input::Text("автоответ"), // i18n-ok: ввод сценария
    until!(|m, cx| palette_shown(m, cx)),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Text("правила"), // i18n-ok: ввод сценария
];

fn palette_shown(m: &Mailer, cx: &App) -> bool {
    m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() > 0)
}

const CARD_BUTTONS: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        m.set_section(crate::Section::Contacts, cx);
        m.contacts.update(cx, |c, cx| {
            c.set_source(crate::contacts::Source::Directory, cx);
            c.select_first(cx);
        });
        Ok(())
    }),
    until!(|m, cx| m.contacts.read(cx).selected_email().is_some()),
];

const KEY_SCHEME: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Keys), window, cx);
        Ok(())
    }),
    Input::Press("settings-keys-scheme-single"),
    until!(|_, cx| crate::prefs::get(cx).key_scheme == postman_core::prefs::KeyScheme::Single),
    Input::Press("settings-keys-scheme-outlook"),
];

const FAR_SEARCH: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(to_calendar),
    until!(|m, cx| m.cal.read(cx).loaded),
    FOCUS,
    Input::Keys(&["ctrl-e"]),
    Input::Typed(|| titled(FAR_EVENT)),
    until!(|m, cx| m.cal.read(cx).results_ready()),
];

const FAR_OPEN: &[Input] = &[Input::Keys(&["down", "enter"])];

fn colleague_week(_: &Scene, m: &Mailer, cx: &App) -> bool {
    event_shown(m, cx, &titled(COLLEAGUE_EVENT))
}

const OWN_FOLD: &[Input] = &[Input::Until(mail_ready), until!(|_, _| screens::spotted("own-account").is_some()), Input::Press("own-account")];

const LEAVE_AND_BACK: &[Input] =
    &[FOCUS, Input::Keys(&["ctrl-2"]), until!(|m, _| m.section == crate::Section::Calendar), FOCUS, Input::Keys(&["ctrl-1"])];

fn branch_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let f = m.folders.read(cx);
    m.palette.is_none() && f.accounts.iter().any(|a| a.name.contains("shared-sales") && !f.is_collapsed(&a.id))
}

/// Ветка свёрнута, открытие папки из неё разворачивает (то же делает выбор папки в палитре, `folders.rs`).
const OPEN_BRANCH: &[Input] = &[
    Input::Run(|_, m, _, cx| {
        let id = m.folders.read(cx).accounts.iter().find(|a| a.name.contains("shared-sales")).map(|a| a.id.clone()).ok_or("no shared account")?;
        m.folders.update(cx, |f, cx| {
            f.collapsed.insert(id);
            cx.notify();
        });
        Ok(())
    }),
    Input::Run(|_, m, _, cx| open_shared(m, cx, "shared-sales")),
];

// --- S7-07б: своя встреча ---

const OWN_MEETING: &str = "Своя встреча"; // i18n-ok: ввод сценария
const OWN_EDITED: &str = "Своя встреча перенесена"; // i18n-ok: ввод сценария
const UPDATED: &str = "Изменения отправлены"; // i18n-ok: начало тоста

fn own_buttons() -> bool {
    screens::spotted("own-edit").is_some() && screens::spotted("own-cancel").is_some()
}

fn select_event(m: &mut Mailer, cx: &mut Context<Mailer>, word: &str) -> Result<(), String> {
    let title = titled(word);
    m.cal.update(cx, |c, cx| c.select_titled(&title, cx));
    if m.cal.read(cx).selected() { Ok(()) } else { Err(format!("no event {title}")) }
}

const OWN_NEW: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(open_meeting),
    until!(|m, _| meeting_view(m).is_some()),
    Input::Press("meeting-title"),
    Input::Typed(|| titled(OWN_MEETING)),
    Input::Keys(&["ctrl-shift-b"]),
    Input::Typed(|| format!("{};", colleague("POSTMAN_STAND_PEER"))),
    Input::Keys(&["ctrl-enter"]),
    until!(|m, _| meeting_view(m).is_none()),
    Input::Run(to_calendar),
];

const OWN_SELECT: &[Input] = &[Input::Run(|_, m, _, cx| select_event(m, cx, OWN_MEETING)), until!(|_, _| own_buttons())];

const FOREIGN_SELECT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(to_calendar),
    until!(|m, cx| event_shown(m, cx, &titled(OWN_MEETING))),
    Input::Run(|_, m, _, cx| select_event(m, cx, OWN_MEETING)),
    // Подробности нарисованы: в панели есть свои кнопки ответа или их нет — смотрит условие сцены.
    until!(|_, _| screens::spotted("event-details").is_some()),
];

const OWN_EDIT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(to_calendar),
    until!(|m, cx| event_shown(m, cx, &titled(OWN_MEETING))),
    Input::Run(|_, m, _, cx| select_event(m, cx, OWN_MEETING)),
    Input::Press("own-edit"),
    until!(|m, _| meeting_view(m).is_some()),
    Input::Press("meeting-title"),
    Input::Keys(&["ctrl-a", "backspace"]),
    Input::Typed(|| titled(OWN_EDITED)),
    until!(|m, cx| meeting_view(m).is_some_and(|v| v.read(cx).model.title == titled(OWN_EDITED))),
    Input::Keys(&["ctrl-enter"]),
    until!(|m, _| meeting_view(m).is_none()),
    Input::Run(to_calendar),
];

const OWN_CANCEL: &[Input] = &[
    until!(|m, cx| event_shown(m, cx, &titled(OWN_EDITED))),
    Input::Run(|_, m, _, cx| select_event(m, cx, OWN_EDITED)),
    Input::Press("own-cancel"),
    Input::Press("own-no"),
    until!(|_, _| screens::spotted("own-no").is_none()),
    Input::Press("own-cancel"),
    Input::Press("own-yes"),
];

// --- письма с файлами: S8-23, T1-13в ---

const IMAGES: &str = "Письмо с картинками"; // i18n-ok: тема `peer.py`
const FILE_BOOK: &str = "Файл книга"; // i18n-ok: тема `peer.py`
const FILE_SLIDES: &str = "Файл слайды"; // i18n-ok: тема `peer.py`
const FILE_CSV: &str = "Файл таблица"; // i18n-ok: тема `peer.py`
const FILE_EML: &str = "Файл письмо"; // i18n-ok: тема `peer.py`
const FILE_MD: &str = "Файл разметка"; // i18n-ok: тема `peer.py`
const FILE_DOCX: &str = "Файл документ"; // i18n-ok: тема `peer.py`

const IMAGES_OPEN: &[Input] =
    &[Input::Until(mail_ready), until!(|m, cx| placed(m, cx, IMAGES).is_some()), Input::Click(|m, _, cx| placed(m, cx, IMAGES), Held::No)];

fn preview_is(m: &Mailer, cx: &App, what: impl Fn(&crate::preview::Shown) -> bool) -> bool {
    use crate::preview::PvState;
    screens::reading(m, cx).and_then(|r| r.preview.as_ref()).is_some_and(|p| matches!(&p.state, PvState::Ready(s) if what(s)))
}

fn preview_ready(m: &Mailer, cx: &App) -> bool {
    preview_is(m, cx, |_| true)
}

/// Разобран разделами: листы, слайды, текст документа.
fn sections_in(m: &Mailer, cx: &App, at_least: usize) -> bool {
    preview_is(m, cx, |s| matches!(s, crate::preview::Shown::Sections { sections, .. } if sections.len() >= at_least))
}

// --- T1-17: переслать вложениями ---

const FORWARD_A: &str = "Пересылка раз"; // i18n-ok: тема `peer.py`
const FORWARD_B: &str = "Пересылка два"; // i18n-ok: тема `peer.py`
const FORWARD_C: &str = "Пересылка три"; // i18n-ok: тема `peer.py`
const FORWARDED: &str = "Три письма вложениями"; // i18n-ok: ввод сценария

fn start_clock(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    *lock_clock() = Some(Instant::now());
    Ok(())
}

fn clock_past(secs: u64) -> bool {
    lock_clock().is_some_and(|t| t.elapsed() > Duration::from_secs(secs))
}

const FORWARD_MANY: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, FORWARD_A).is_some() && placed(m, cx, FORWARD_B).is_some() && placed(m, cx, FORWARD_C).is_some()),
    // Сверка сразу после входа ещё двигает строки: щелчок попал бы по соседней.
    Input::Run(start_clock),
    until!(|_, _| clock_past(2)),
    Input::Click(|m, _, cx| placed(m, cx, FORWARD_A), Held::No),
    until!(|m, cx| picked(m, cx, FORWARD_A).is_some()),
    Input::Click(|m, _, cx| placed(m, cx, FORWARD_B), Held::Ctrl),
    // Панель выделения сдвигает строки вниз: место следующей строки — после неё.
    until!(|m, cx| m.list.read(cx).selection.len() >= 2),
    Input::Click(|m, _, cx| placed(m, cx, FORWARD_C), Held::Ctrl),
    until!(|m, cx| m.list.read(cx).selection.len() >= 3),
    Input::Press("bulk-forward"),
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| {
        let model = &c.read(cx).model;
        model.subject.is_empty() && model.attachments().count() == 3 && model.attachments().all(|(_, a)| a.name.ends_with(".eml"))
    })),
    Input::Typed(|| format!("{};", colleague("POSTMAN_USER"))),
    Input::Keys(&["tab"]),
    Input::Typed(|| titled(FORWARDED)),
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| c.read(cx).model.subject == titled(FORWARDED))),
    Input::Press("send"),
];

const FORWARD_ONE: &[Input] = &[
    Input::Until(mail_ready),
    until!(|m, cx| placed(m, cx, FORWARD_A).is_some()),
    Input::Right(|m, _, cx| placed(m, cx, FORWARD_A)),
    until!(|m, _| m.menu.is_some()),
    Input::Keys(&["down", "down", "down", "enter"]),
    until!(|m, _| screens::compose_tab(m).is_some()),
];

// --- S5-12 Ф4, шаг 9, T1-02б, шаг 5, T1-23, S6-11 ---

fn own_no_cert(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let me = colleague("POSTMAN_USER");
    screens::compose_tab(m).is_some_and(|c| {
        c.read(cx).model.checks().any(|k| matches!(k, postman_core::checks::Check::NoCertificate { addresses, .. } if addresses.contains(&me)))
    })
}

const OWN_CERT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        // Адресат без сертификата заслонил бы свой: замечание о себе — когда у адресатов сертификаты есть или письмо себе.
        let draft = postman_compose::Draft { to: vec![postman_compose::Address::new("", colleague("POSTMAN_USER"))], ..Default::default() };
        m.open_compose(draft, cx);
        Ok(())
    }),
    until!(|m, _| screens::compose_tab(m).is_some()),
    Input::Press("tb-mark"),
    Input::Press("mark-conf"),
];

fn plain_login(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.settings.add.as_ref().is_some_and(|p| p.read(cx).step == crate::signin::Step::Login)
        && screens::spotted("signin-next").is_some()
        && screens::spotted("signin-windows").is_none()
}

const PLAIN_LOGIN: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(accounts_page),
    Input::Press("settings-account-add"),
    until!(|m, cx| add_panel(m, cx).is_some()),
    Input::Typed(|| colleague("POSTMAN_URL")),
    Input::Keys(&["enter"]),
];

const CONTACT_MENU: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        m.set_section(crate::Section::Contacts, cx);
        Ok(())
    }),
    until!(|m, _| m.section == crate::Section::Contacts && screens::spots_numbered("contact-").len() >= 2),
    Input::Run(|_, m, _, cx| {
        m.contacts.update(cx, |c, cx| c.set_source(crate::contacts::Source::Directory, cx));
        Ok(())
    }),
    until!(|_, _| screens::spots_numbered("contact-").len() >= 2),
    Input::Click(|_, _, _| screens::spots_numbered("contact-").first().map(|b| b.center()), Held::No),
    until!(|m, cx| m.contacts.read(cx).selected_email().is_some()),
    Input::Right(|_, _, _| screens::spots_numbered("contact-").get(1).map(|b| b.center())),
    until!(|m, cx| m.contacts.read(cx).menu_open()),
    Input::Press("contact-menu-item-2"),
    until!(|m, cx| !m.contacts.read(cx).menu_open() && m.toasts.read(cx).latest().is_some()),
    Input::Right(|_, _, _| screens::spots_numbered("contact-").get(1).map(|b| b.center())),
    until!(|m, cx| m.contacts.read(cx).menu_open()),
    Input::Press("contact-menu-item-1"),
];

const MINE_MENU: &[Input] = &[
    Input::Until(mail_ready),
    Input::Run(|_, m, _, cx| {
        m.set_section(crate::Section::Contacts, cx);
        m.contacts.update(cx, |c, cx| c.set_source(crate::contacts::Source::Mine, cx));
        Ok(())
    }),
    until!(|_, _| !screens::spots_numbered("contact-").is_empty()),
    Input::Right(|_, _, _| screens::spots_numbered("contact-").first().map(|b| b.center())),
    until!(|m, cx| m.contacts.read(cx).menu_open()),
    Input::Press("contact-menu-item-3"),
];

const DELEG_BY_ME: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| {
        m.tasks.update(cx, |t, cx| t.set_nav(Nav::Delegations(Tab::ByMe), cx));
        Ok(())
    }),
];

const DELEG_TO_ME: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    until!(|m, cx| task(m, cx, ORDER).is_some()),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::Delegations(Tab::ToMe), ORDER)),
];

const DELEG_FOREIGN: &[Input] = &[FOCUS, Input::Keys(&["delete"])];

const DELEG_DELETE: &[Input] = &[
    Input::Until(mail_ready),
    OPEN_TASKS,
    Input::Until(tasks_ready),
    Input::Run(|_, m, _, cx| pick(m, cx, Nav::Delegations(Tab::ByMe), ORDER)),
    FOCUS,
    Input::Keys(&["delete"]),
    until!(|m, cx| task(m, cx, ORDER).is_none()),
    Input::Press("notice-act"),
];
