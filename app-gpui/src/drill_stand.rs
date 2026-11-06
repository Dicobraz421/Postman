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
    // S6-06, шаг 4: второй Postman: правка пришла сама.
    scene("st-s606-seen", |_, m, cx| task(m, cx, EDITED_TASK).is_some_and(|t| t.important), SECOND_WINDOW),
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
    // S7-04, шаг 9: «Отменить» — черновик снова открыт, письмо не ушло.
    scene("st-s704-undo", |_, m, _| screens::compose_tab(m).is_some() && screens::spotted("send-later").is_none(), IMAP_UNDO_SEND),
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

fn vacation_saved(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.settings.bar.is_some() && vacation(m, cx).is_some_and(|p| !p.busy && !p.dirty() && p.form.enabled)
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

const SECOND_WINDOW: &[Input] = &[Input::Until(mail_ready), OPEN_TASKS, Input::Until(tasks_ready)];

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
    Input::Keys(&["ctrl-e"]),
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
const IMAP_UNDO: &str = "Отмена IMAP"; // i18n-ok: ввод сценария
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
    Input::Press("delete-yes"),
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
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| matches!(c.read(cx).model.status, crate::compose::Status::Busy(_)))),
    Input::Until(|_, m, cx| draft_saved(m, cx)),
];

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
    until!(|m, _| screens::compose_tab(m).is_none()),
    until!(|_, _| screens::spotted("notice-act").is_some()),
    Input::Press("notice-act"),
    until!(|m, _| screens::compose_tab(m).is_some()),
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

const DAV_ANSWER: &[Input] = &[Input::Until(mail_ready), Input::Keys(&["ctrl-2"])];

const DAV_CONTACT_NEW: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-3"]),
    until!(|m, _| m.section == crate::Section::Contacts),
    Input::Keys(&["ctrl-shift-c"]),
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

const DAV_SUGGEST: &[Input] =
    &[Input::Until(mail_ready), Input::Keys(&["ctrl-n"]), until!(|m, _| screens::compose_tab(m).is_some()), Input::Typed(|| STAMP.clone())];
