//! Сценарии с вводом для задач, поручений, общих ящиков, локальных папок и экспорта (`docs/manual-tests/`): имя сцены —
//! `task-<карточка>-<что>`. Механика — `screens.rs`, запуск — тест `task_drills` в `app-gpui/tests/manual.rs`. `ready` и `Until`
//! проверяют обещанное шагом по модели; цвет и вид — по снимку. Демо: письма не уходят, сервера нет; диалоги Windows
//! обходятся шагом `Input::Run` и в сценарии не проверяются.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui_kit::*;
use jiff::civil::{Date, Time};
use pets_i18n::tr;
use postman_calendar::assign::{self, Status};
use postman_calendar::{Anchor, Moment, Task, Trigger};
use postman_core::assign::{Bucket, Tab};
use postman_core::export::Format;
use postman_core::mailtask;
use postman_core::prefs::{Accent, KeyScheme, Prefs};
use postman_core::recur::{self, Freq, Rule};
use postman_core::tasks::{ChipKind, List, Reply};
use postman_demo::delegations as d;
use postman_demo::tasks as demo;

use crate::export::What;
use crate::preview::{PvState, Shown};
use crate::screens::{self, Held, Input, Scene, rows_loaded};
use crate::settings_tab::Section as Page;
use crate::shared::Act;
use crate::signin::Step;
use crate::tabs::TabKind;
use crate::tasks::{Edit, Field, Group, Item, Nav, TasksView};
use crate::{Mailer, Section};

pub(crate) const PART: crate::part::Part = crate::part::Part { name: "drill-tasks", scenes: SCENES, reset: Some(reset), ..crate::part::Part::NONE };

/// Шаг со своим условием.
macro_rules! until {
    (|$m:tt, $cx:tt| $done:expr) => {
        Input::Until(|_, $m, $cx| $done)
    };
}

/// Выбрать задачу, как щелчок по строке.
macro_rules! pick {
    ($title:expr) => {
        Input::Run(|_, m, _, cx| {
            m.tasks.update(cx, |t, cx| t.select_title($title, cx));
            if m.tasks.read(cx).selected_task().is_some_and(|t| t.title == $title) { Ok(()) } else { Err(format!("no task {}", $title)) }
        })
    };
}

/// Открыть раздел задач, как щелчок слева.
macro_rules! nav {
    ($nav:expr) => {
        Input::Run(|_, m, _, cx| {
            m.tasks.update(cx, |t, cx| t.set_nav($nav, cx));
            Ok(())
        })
    };
}

const PROMOTE: &[Input] = &[
    until!(|m, cx| rows_loaded(m.list.read(cx))),
    Input::Click(|m, _, cx| screens::row_place(m, cx, 0), Held::No),
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && keep(r.row.subject.clone()))),
    Input::Keys(&["insert"]),
    Input::Press("lifecycle-pick-2"),
    until!(|m, _| m.lifecycle.picker.is_none()),
    TASKS,
    IN_TASKS,
    Input::Press("tasks-nav-3"),
    until!(|m, cx| on_list(m, cx, List::FromMail) && shown(m, cx, &kept())),
    Input::Run(|_, m, _, cx| {
        let title = kept();
        m.tasks.update(cx, |t, cx| t.select_title(&title, cx));
        if m.tasks.read(cx).selected_task().is_some_and(mailtask::is_letter) { Ok(()) } else { Err("no letter task".into()) }
    }),
    Input::Press("card-prio"),
];

fn promoted(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.tasks.read(cx).selected_task().is_some_and(|t| !mailtask::is_letter(t) && t.mail.is_some() && t.important)
}

const fn scene(name: &'static str, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, start, ready).input(input).on_request()
}

const SCENES: &[Scene] = &[
    // T2-04: письмо со сроком в «Из почты» — «!» в карточке записывает его задачей со ссылкой на письмо.
    scene("task-t204-promote", promoted, PROMOTE),
    // S6-04, шаг 1: фраза — чипы и итог, Enter — задача со сроком, категорией и «!».
    scene("task-s604-chips", quick_added, CHIPS),
    // S6-04, шаг 2: × на чипе «завтра» — снова часть названия, срок — по времени.
    scene("task-s604-unchip", unchip_added, UNCHIP),
    // S6-04, шаг 3: категория в английской раскладке, «Глава 7.2 договора» без срока.
    scene("task-s604-layout", no_chips, LAYOUT),
    // S6-04, шаг 4: «в пт», «через 3 дня», «15.11», «15 ноября», «утром», «вечером».
    scene("task-s604-phrases", evening_parsed, PHRASES),
    // S6-04, шаг 5 (только `--lang en`): «Call tomorrow at 3pm #clients !».
    scene("task-s604-en", english_parsed, ENGLISH),
    // S6-05, шаг 1: кнопка раздела, списки слева, группы «Сегодня», карточка.
    scene("task-s605-lists", lists_ready, LISTS),
    // S6-05, шаг 2: состав строки: важная, проект, подзадачи, метка срока (вид — по снимку); повтор.
    scene("task-s605-row", row_ready, ROW),
    // S6-05, шаг 3: круг, Space, отмена за 2 с, «Выполненные», «Отменить», «Вернуть в работу».
    scene("task-s605-done", reopened, DONE),
    // S6-05, шаг 4: Ctrl+N, Esc, строка «Добавить задачу», заголовки вкладок, строка состояния, письмо в почте.
    scene("task-s605-new", new_letter, NEW),
    // S6-04: «Добавить задачу» на другой день: задача создана, выбор и карточка прежние.
    scene("task-s604-other-day", kept_selection, OTHER_DAY),
    // Решение владельца п. 9: «Да» при удалении контакта срабатывает с первого щелчка при стопке тостов.
    scene("task-s511-delete", contact_deleted, DELETE_UNDER_TOASTS),
    // S5-11: форма контакта: Tab и Shift+Tab ходят по полям.
    scene("task-s511-tab", form_filled, FORM_TAB),
    // S6-05, шаг 5: Insert, F2, Esc, Del и «Отменить», ↑/↓, Ctrl+E, «!», подзадача.
    scene("task-s605-keys", keys_done, KEYS),
    // S6-05, шаг 6: письмо со сроком в «Из почты»: выполнить, «Отменить», письмо в новой вкладке, папка та же.
    scene("task-s605-mail", letter_tab, FROM_MAIL),
    // S6-06, шаг 1: варианты напоминания, новый проект строкой, категория, Insert.
    scene("task-s606-fields", fields_set, FIELDS),
    // S6-06, шаг 2: подзадачи: Enter, поле готово, отметка, прогресс.
    scene("task-s606-subs", subs_done, SUBS),
    // S6-06, шаг 3: заметки Markdown: текст сохраняется при смене задачи.
    scene("task-s606-notes", notes_back, NOTES),
    // S6-07, шаг 1: восемь вариантов повтора, выбранное — в задаче.
    scene("task-s607-repeat", monthly_set, REPEAT),
    // S6-07, шаг 2: выполнение повторяющейся: копия через месяц, «Отменить» — как было.
    scene("task-s607-next", repeat_undone, NEXT),
    // S6-07, шаг 3: срок 31-го, каждый месяц: следующая — 30-го.
    Scene::new("task-s607-31", with_31st, thirtieth).input(THIRTY_FIRST).on_request(),
    // S6-07, шаг 4: напоминание — тост с «Отложить на 10 мин»; окно не в фокусе: уведомление Windows запрошено, но в прогоне
    // снимков не показано и AUMID в реестр не записан (решение владельца п. 13).
    Scene::new("task-s607-remind", with_reminder, snoozed).input(REMIND).on_request(),
    // S6-07, шаг 5: первым в вариантах — значение из «Настройки → Календарь».
    scene("task-s607-default", default_first, DEFAULT),
    // S6-08, шаги 1, 2: окно, ←/→, кнопки, блок поставлен, «Убрать».
    scene("task-s608-block", block_removed, BLOCK),
    // S6-09, шаг 1: круги всех статусов в проекте (вид — по снимку).
    scene("task-s609-statuses", all_statuses, STATUSES),
    // S6-09, шаг 2: «Вернуть с комментарием»: Esc, Enter; Insert — новый срок.
    scene("task-s609-return", returned, RETURN),
    // S6-09, шаг 2: Space — «Принять результат».
    scene("task-s609-accept", result_accepted, ACCEPT),
    // S6-09, шаг 3: предложенный срок, отклонённое — «Поручить другому».
    scene("task-s609-proposed", reassign_form, PROPOSED),
    // S6-09, шаг 4: «Напомнить» один раз, «Забрать задачу» с «Отменить».
    scene("task-s609-remind", revoke_undone, DELEG_REMIND),
    // S6-09, шаг 5: «Предложить срок», «Отклонить», «Принять», «В работе», «Готово», «Отозвать с проверки».
    scene("task-s609-incoming", recalled, INCOMING),
    // S6-09, шаг 6: форма «Поручить»: имя в другой раскладке, Enter — первая подсказка, комментарий, Enter.
    scene("task-s609-assign", assigned, ASSIGN),
    // S6-11, шаги 1, 2: раздел, вкладки, группы, «Только свои» снят — чужое с чипом.
    scene("task-s611-by-me", watched_shown, BY_ME),
    // S6-11, шаг 3: «Мне поручили»: группы.
    scene("task-s611-to-me", to_me_groups, TO_ME),
    // S6-11, шаги 4, 5 (без письма исполнителю): «Напомнить всем» дважды; «Забрать задачу» без вопроса.
    scene("task-s611-remind", taken_back, REMIND_ALL),
    // S6-11, шаг 6: Del на своём поручении и «Отменить», на чужом — подсказка.
    scene("task-s611-delete", foreign_kept, DELETE),
    // S6-11, шаг 7: «Открыть в почте»: все папки, «Ждём»; строка состояния.
    scene("task-s611-wait", wait_open, WAIT),
    // S5-26, шаг 1: «Поставить категорию», «Выберите категорию.», текст правила (демо не сохраняет: строка — из правила).
    scene("task-s526-rule", rule_line, RULE),
    // S5-28, шаг 1: подсказки веток общих ящиков.
    scene("task-s528-hints", boss_hint, HINTS),
    // S5-28, шаг 2: кнопка «Взять в работу» с подсказкой Ctrl+J в общем ящике, в своих «Входящих» её нет.
    scene("task-s528-button", own_no_button, BUTTON),
    // S5-28, шаг 3: метки «Назначено: …» у 2, 3, 5 писем.
    scene("task-s528-marks", marks_shown, MARKS),
    // S5-28, шаг 4: Ctrl+J: тост, метка, кнопка; «Отменить» — как было.
    scene("task-s528-take", take_undone, TAKE),
    // S5-28, шаг 9: полоса, метка и кнопка письма в работе у меня (обе темы — по снимку).
    scene("task-s528-strip", strip_shown, STRIP),
    // S5-28, шаг 5: «Забрать себе», затем Ctrl+J — «Вернуть в очередь».
    scene("task-s528-steal", released, STEAL),
    // S5-28, шаг 6: ответ от ящика, строки под текстом, подсказки меню «От», свой адрес.
    scene("task-s528-reply", reply_personal, REPLY),
    // S5-28, шаг 7: ящик только для чтения и свои «Входящие»: тосты Ctrl+J.
    scene("task-s528-readonly", own_take_toast, READ_ONLY),
    // S5-28, шаг 10: подсказка «Проверка отзыва сертификатов».
    scene("task-s528-revocation", revocation_hint, REVOCATION),
    // S5-29, шаг 1: раздел «Автоответ», закладки, «Отмена» и «Сохранить» (вид в обеих темах — по снимку).
    scene("task-s529-look", vacation_ready, VACATION),
    // S5-30, шаг 1: порядок разделов, поиск «пароль», «шрифт», «mailto».
    scene("task-s530-order", mailto_found, SECTIONS),
    // S5-30, шаг 7: акцент, размер шрифта, плотность, аватары и превью, KeyTips и задержка.
    scene("task-s530-look", look_done, LOOK),
    // S8-15, шаг 1: группа, подсказка, сворачивание.
    tree_scene("task-s815-group", group_open, GROUP),
    // S8-15, шаг 2: новая, вложенная, переименование, удаление, перетаскивание локальных папок.
    tree_scene("task-s815-edit", back_to_root, LOCAL_EDIT),
    // S8-15, шаги 5, 6 (без тела из базы и поиска), S8-16, шаг 1: письма в «На этом компьютере» без вопроса, письмо закрыто, выделение снято.
    tree_scene("task-s815-move", local_opened, TO_LOCAL),
    // S8-32, шаг 1: полоса экспорта, «С подпапками», «Отмена», Esc, меню папки, группа «На этом компьютере».
    scene("task-s832-bar", local_asked, EXPORT_BAR),
    // S8-33б, шаг 2: EML, mbox, Maildir, PST; MSG нет.
    scene("task-s833-formats", pst_chosen, EXPORT_FORMATS),
    // S8-32, шаги 3, 8 (каталог — без диалога Windows): mbox, файлы на диске, прошлый формат — по умолчанию.
    scene("task-s832-mbox", format_kept, EXPORT_MBOX),
    // S8-33б, шаги 3, 4 (каталог — без диалога Windows, маленькая папка): один файл, повтор — «(2)», без `*.tmp`.
    scene("task-s833-pst", pst_files, EXPORT_PST),
    // S8-32, шаг 6, S8-33б, шаг 6 (каталог — без диалога Windows): «Остановить», недописанного нет.
    scene("task-s832-stop", export_stopped, EXPORT_STOP),
    // D4: элемент с фокусом ушёл с экрана — клавиши разделов и Ctrl+Shift+Q всё равно доходят.
    scene("task-t1-focus-lost", meeting_tab, FOCUS_LOST),
    // T1-06: своя группа и ветка сворачиваются, смена раздела их не разворачивает, переход к папке — разворачивает.
    scene("task-t106-groups", branch_opened, GROUPS),
    // T1-09: открытие на начале рабочего дня, 00:00…24:00, событие в 07:00, Home.
    scene("task-t109-day", back_at_work, WHOLE_DAY),
    // T1-10, шаги 1, 2 и T1-10в, шаги 1, 3 (в демо, вид — по снимку): логотип в размере из HTML, большая картинка по колонке.
    scene("task-t110-images", images_fit, IMAGES),
    // T1-22, шаги 1–3 в демо: «Отправленные»: открыть, ↑/↓, выделение, флажок, «В папку».
    scene("task-t122-sent", sent_done, SENT),
    // T1-23, шаги 1, 2: меню на другой строке, «Копировать адрес», «Написать», «Встреча».
    scene("task-t123-menu", meeting_with_contact, ROW_MENU),
    // T1-23, шаги 3, 4: Shift+F10, Home/End, Delete и F2 в меню не действуют, «Изменить», «Удалить».
    scene("task-t123-keys", delete_asked, CONTACT_KEYS),
    // S5-21, шаги 5, 6: приглашение выбрано (контур, пунктир, кнопки — по снимку), подсказка значка ответов.
    scene("task-s521-event", legend_hint, WEEK),
    // S5-22, шаг 1: у коллег свой цвет, события — подложкой цвета календаря и с именем владельца, свои — синие, категория — полосой.
    scene("task-s522-owners", owners_shown, OWNERS),
    // S5-22, шаги 1, 2: цвета календарей коллег; выбор сохраняется.
    scene("task-s522-colors", color_saved, COLORS),
    // S5-22, шаг 2, второй запуск: выбранный цвет на месте.
    scene("task-s522-kept", color_saved, COLORS_KEPT),
    // Раздел 24, шаг 9 (без сервера): три шага, «Назад», «Изменить», «Хранить» и «Не хранить», отказ на шаге «Пароль».
    scene("task-24-firstrun", refused, FIRST_RUN),
    // Раздел 38, шаг 2: подсказка при наведении на сетку.
    scene("task-38-tip", grid_tip, GRID_TIP),
    // Раздел 41, шаг 3: подсказка «Принять (A)» при наведении на кнопку.
    scene("task-41-tip", invite_tip, INVITE_TIP),
    // S8-23, шаги 1–3, 5 в демо: книга, презентация, CSV, TSV, RTF, Markdown, ODT — образцы собираются сценарием.
    scene("task-s823-sections", odt_shown, SHEETS),
    // T1-13в, шаги 1–3 в демо: `.docx` текстом, Esc, `.docm` — карточка проверки.
    scene("task-t113-word", macro_card, WORD),
    // T1-13в, шаг 3 настоящим путём: щелчок по карточке `.xlsm` — проверка в воркере, карточка с макросами.
    scene("task-t113-xlsm", xlsm_card, XLSM),
    // S8-27, шаг 1 (без Проводника): раздел «Просмотр вложений», кнопка папки модулей.
    scene("task-s827-viewers", viewers_page, VIEWERS),
    // T1-18, шаг 3 (без перетаскивания ОС): короткий щелчок по карточке — просмотр.
    scene("task-t118-click", card_opens_preview, OUTLOOK),
    // S6-09, шаг 7: письмо с `task.ics`: карточка, вложение не дублируется, «Принять», «Открыть в задачах».
    scene("task-s609-letter", letter_task_open, TASK_LETTER),
    // S8-27, шаг 2 (запуск с модулем в папке): строка модуля; `.fb2` идёт модулю.
    scene("task-s827-module", module_listed, MODULE),
];

// Общие шаги.

const TASKS: Input = Input::Keys(&["ctrl-4"]);
const IN_TASKS: Input = until!(|m, cx| m.section == Section::Tasks && m.tasks.read(cx).ready());
const ENTER: Input = Input::Keys(&["enter"]);
const ALL: Input = Input::Keys(&["ctrl-a", "backspace"]);
const PICK_UNDO: Input = Input::Press("notice-act");
const READY_MAIL: Input = until!(|m, cx| rows_loaded(m.list.read(cx)));

/// Обычная задача без подзадач: её выполняют и переименовывают сценарии.
const PLAIN: &str = "Проверить счета подрядчиков за месяц";
const REPEATING: &str = demo::REPEAT;
const WORK: &str = "Ресурсный план группы на ноябрь";
const DECLINED: &str = "Заказать пропуска для аудиторов";
const PROPOSING: &str = "Требования к мониторингу для ТЗ";

fn start(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    Ok(())
}

/// Сцены этой части: другим — их демо-задачи и настройки.
fn mine(name: &str) -> bool {
    name.strip_prefix("task-").is_some_and(|r| r.starts_with(['s', '2', '3', '4']) || r.starts_with("t1"))
}

/// Перед сценой: задачи демо заново, «Сегодня».
fn reset(s: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) {
    if !mine(s.name) {
        return;
    }
    let base = Prefs::default();
    crate::prefs::set_accent(base.look.accent, cx);
    crate::prefs::set_density(base.general.density.unwrap_or(24), cx);
    crate::prefs::update(cx, |p| {
        p.calendar.reminder = base.calendar.reminder;
        p.look = base.look.clone();
        p.keytips = base.keytips;
        p.key_scheme = base.key_scheme;
    });
    // Тосты прошлой сцены: их «Отменить» перехватило бы щелчок.
    m.toasts.update(cx, |t, cx| t.hide_all(cx));
    screens::in_background(false);
    m.live.signin = None;
    m.cal.update(cx, |c, cx| c.today(cx));
    m.folders.update(cx, |f, _| f.own = None);
    // Поле прошлой сцены могло уйти с экрана: клавиши — снова экрану.
    window.focus(&m.focus, cx);
    let tz = crate::prefs::get(cx).calendar.zone();
    let tasks = crate::tasks::demo(jiff::Timestamp::now().as_second(), &tz);
    m.tasks.update(cx, |t, cx| {
        t.own_only = true;
        t.draft = None;
        t.title = None;
        t.open = None;
        t.due_open = false;
        t.deleg.edit = None;
        t.completing = Default::default();
        t.on_reply(&Reply::Loaded(tasks), cx);
        t.set_tab(Tab::ByMe, cx);
        t.set_nav(Nav::List(List::Today), cx);
        t.escape(window, cx);
        t.set_quick("", window, cx);
    });
}

fn view<'a>(m: &'a Mailer, cx: &'a App) -> &'a TasksView {
    m.tasks.read(cx)
}

fn task<'a>(m: &'a Mailer, cx: &'a App, title: &str) -> Option<&'a Task> {
    view(m, cx).tasks.iter().find(|t| t.title == title)
}

fn picked<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a Task> {
    view(m, cx).selected_task()
}

fn picked_is(m: &Mailer, cx: &App, title: &str) -> bool {
    picked(m, cx).is_some_and(|t| t.title == title)
}

fn shown(m: &Mailer, cx: &App, title: &str) -> bool {
    view(m, cx).shown.iter().any(|t| t.title == title)
}

fn toast(m: &Mailer, cx: &App, text: &str) -> bool {
    m.toasts.read(cx).latest().is_some_and(|t| t == text)
}

fn toast_starts(m: &Mailer, cx: &App, text: &str) -> bool {
    m.toasts.read(cx).latest().is_some_and(|t| t.starts_with(text))
}

fn zone(cx: &App) -> jiff::tz::TimeZone {
    crate::prefs::get(cx).calendar.zone()
}

fn today(cx: &App) -> Date {
    jiff::Zoned::now().with_time_zone(zone(cx)).date()
}

fn after(day: Date, n: i64) -> Date {
    day.checked_add(jiff::Span::new().days(n)).unwrap_or(day)
}

fn due_day(t: &Task, cx: &App) -> Option<Date> {
    t.due.as_ref().and_then(|d| d.date_in(&zone(cx)))
}

fn due_time(t: &Task) -> Option<Time> {
    t.due.as_ref().map(|d| d.local.time())
}

/// Заголовки групп списка по порядку.
fn heads(m: &Mailer, cx: &App) -> Vec<Group> {
    view(m, cx).items.iter().filter_map(|i| if let Item::Head(g, _) = i { Some(*g) } else { None }).collect()
}

/// Строка задачи в списке.
fn row_of(m: &Mailer, cx: &App, title: &str) -> Option<usize> {
    let v = view(m, cx);
    v.items.iter().position(|i| matches!(i, Item::Row(r) if v.shown.get(*r).is_some_and(|t| t.title == title)))
}

fn subtasks(m: &Mailer, cx: &App, title: &str) -> (usize, usize) {
    task(m, cx, title).map_or((0, 0), |t| postman_core::tasks::subtask_progress(&view(m, cx).tasks, &t.uid))
}

static KEPT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SINCE: Mutex<Option<Instant>> = Mutex::new(None);

/// Запомнить значение на шаг вперёд (условия без состояния); всегда `true`.
fn keep(v: impl Into<String>) -> bool {
    *KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = vec![v.into()];
    true
}

fn kept() -> String {
    KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first().cloned().unwrap_or_default()
}

/// Прошло `ms` от первого вызова: шаг «ничего не случилось» — не сразу.
fn pause(ms: u64) -> bool {
    let mut since = SINCE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let at = *since.get_or_insert_with(Instant::now);
    let done = at.elapsed() >= Duration::from_millis(ms);
    if done {
        *since = None;
    }
    done
}

// S6-04.

const QUICK: Input = Input::Press("tasks-quick");

const CHIPS: &[Input] = &[
    TASKS,
    IN_TASKS,
    QUICK,
    Input::Text("Отправить КП завтра 10:00 #Клиенты !"),
    until!(|m, cx| {
        let p = &view(m, cx).parsed;
        let kinds = [ChipKind::Due, ChipKind::Category, ChipKind::Important];
        let all = kinds.iter().all(|k| p.chips.iter().any(|c| c.kind == *k));
        all && p.title == "Отправить КП" && p.date == Some(after(today(cx), 1)) && p.important
    }),
    ENTER,
];

fn quick_added(_: &Scene, m: &Mailer, cx: &App) -> bool {
    task(m, cx, "Отправить КП").is_some_and(|t| {
        let time = due_time(t) == Some(jiff::civil::time(10, 0, 0, 0));
        due_day(t, cx) == Some(after(today(cx), 1)) && time && t.important && t.keywords.iter().any(|k| k == "pm.cat.clients")
    }) && view(m, cx).quick.read(cx).value().is_empty()
}

const UNCHIP: &[Input] = &[
    TASKS,
    IN_TASKS,
    QUICK,
    Input::Text("Созвон завтра 10:00"),
    until!(|m, cx| view(m, cx).parsed.chips.len() == 2),
    Input::Press("unchip-0"),
    until!(|m, cx| view(m, cx).parsed.title.contains("завтра") && view(m, cx).parsed.chips.len() == 1),
    ENTER,
];

/// Срок — по времени: сегодня, если 10:00 ещё впереди, иначе завтра.
fn unchip_added(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let now = jiff::Zoned::now().with_time_zone(zone(cx));
    let day = if now.time() < jiff::civil::time(10, 0, 0, 0) { now.date() } else { after(now.date(), 1) };
    task(m, cx, "Созвон завтра").is_some_and(|t| due_day(t, cx) == Some(day) && due_time(t) == Some(jiff::civil::time(10, 0, 0, 0)))
}

const LAYOUT: &[Input] = &[
    TASKS,
    IN_TASKS,
    QUICK,
    Input::Text("Звонок #rkbtyns"),
    until!(|m, cx| view(m, cx).parsed.category.as_deref() == Some("pm.cat.clients")),
    ALL,
    Input::Text("Глава 7.2 договора"),
];

fn no_chips(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let p = &view(m, cx).parsed;
    p.chips.is_empty() && p.date.is_none() && p.title == "Глава 7.2 договора"
}

const PHRASES: &[Input] = &[
    TASKS,
    IN_TASKS,
    QUICK,
    Input::Text("Сверка в пт"),
    until!(|m, cx| view(m, cx)
        .parsed
        .date
        .is_some_and(|d| { d.weekday() == jiff::civil::Weekday::Friday && d >= today(cx) && d <= after(today(cx), 7) })),
    ALL,
    Input::Text("Сверка через 3 дня"),
    until!(|m, cx| view(m, cx).parsed.date == Some(after(today(cx), 3))),
    ALL,
    Input::Text("Сверка 15.11"),
    until!(|m, cx| view(m, cx).parsed.date.is_some_and(|d| d.month() == 11 && d.day() == 15)),
    ALL,
    Input::Text("Сверка 15 ноября"),
    until!(|m, cx| view(m, cx).parsed.date.is_some_and(|d| d.month() == 11 && d.day() == 15)),
    ALL,
    Input::Text("Сверка утром"),
    until!(|m, cx| view(m, cx).parsed.time.is_some_and(|t| i32::from(t.hour()) * 60 == i32::from(crate::prefs::get(cx).calendar.work_start))),
    ALL,
    Input::Text("Сверка вечером"),
];

fn evening_parsed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).parsed.time.is_some_and(|t| i32::from(t.hour()) * 60 == i32::from(crate::prefs::get(cx).calendar.work_end))
}

const ENGLISH: &[Input] = &[TASKS, IN_TASKS, QUICK, Input::Text("Call tomorrow at 3pm #clients !")];

fn english_parsed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let p = &view(m, cx).parsed;
    let at = p.time == Some(jiff::civil::time(15, 0, 0, 0));
    p.title == "Call" && p.date == Some(after(today(cx), 1)) && at && p.category.as_deref() == Some("pm.cat.clients") && p.important
}

// S6-05.

fn on_list(m: &Mailer, cx: &App, l: List) -> bool {
    view(m, cx).nav == Nav::List(l)
}

const LISTS: &[Input] = &[
    Input::Press("nav-tasks"),
    IN_TASKS,
    Input::Press("tasks-nav-0"),
    until!(|m, cx| on_list(m, cx, List::Inbox)),
    Input::Press("tasks-nav-2"),
    until!(|m, cx| on_list(m, cx, List::Upcoming) && !view(m, cx).shown.is_empty()),
    Input::Press("tasks-nav-4"),
    until!(|m, cx| on_list(m, cx, List::Someday) && shown(m, cx, demo::SOMEDAY)),
    Input::Press("tasks-nav-5"),
    until!(|m, cx| on_list(m, cx, List::Done) && view(m, cx).shown.iter().all(|t| t.progress.is_closed())),
    Input::Press("tasks-nav-1"),
];

/// «Просрочено» сверху, дальше — по времени и без времени (час запуска решает, какие из них есть).
fn lists_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let order = [Group::Overdue, Group::TodayTimed, Group::NoTime];
    let places: Vec<Option<usize>> = heads(m, cx).iter().map(|g| order.iter().position(|o| o == g)).collect();
    let sorted = places.windows(2).all(|w| w[0] < w[1]) && places.iter().all(Option::is_some);
    on_list(m, cx, List::Today) && places.first() == Some(&Some(0)) && sorted && picked(m, cx).is_some()
}

const ROW: &[Input] = &[
    TASKS,
    IN_TASKS,
    until!(|m, cx| row_of(m, cx, demo::MAIN).is_some_and(|i| screens::spotted(&format!("task-{i}")).is_some())),
    Input::Click(|m, _, cx| row_of(m, cx, demo::MAIN).and_then(|i| screens::spotted(&format!("task-{i}"))), Held::No),
];

fn row_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let badge = task(m, cx, PLAIN).and_then(|t| crate::tasks::due_badge(t, jiff::Timestamp::now().as_second(), &zone(cx)));
    let main = task(m, cx, demo::MAIN).is_some_and(|t| t.important && t.project.is_some() && !t.notes.is_empty());
    let repeats = task(m, cx, REPEATING).is_some_and(|t| recur::rule_of(t).is_some());
    picked_is(m, cx, demo::MAIN) && main && badge.is_some() && repeats && subtasks(m, cx, demo::MAIN) == (2, 4)
}

fn pending(m: &Mailer, cx: &App, title: &str) -> bool {
    task(m, cx, title).is_some_and(|t| view(m, cx).completing.is_pending(&t.uid))
}

fn closed(m: &Mailer, cx: &App, title: &str) -> bool {
    task(m, cx, title).is_some_and(|t| t.progress.is_closed())
}

fn open(m: &Mailer, cx: &App, title: &str) -> bool {
    task(m, cx, title).is_some_and(|t| !t.progress.is_closed())
}

const DONE: &[Input] = &[
    TASKS,
    IN_TASKS,
    pick!(PLAIN),
    until!(|m, cx| row_of(m, cx, PLAIN).is_some_and(|i| screens::spotted(&format!("task-check-{i}")).is_some())),
    Input::Click(|m, _, cx| row_of(m, cx, PLAIN).and_then(|i| screens::spotted(&format!("task-check-{i}"))), Held::No),
    until!(|m, cx| pending(m, cx, PLAIN) && shown(m, cx, PLAIN)),
    Input::Keys(&["space"]),
    until!(|m, cx| !pending(m, cx, PLAIN) && open(m, cx, PLAIN)),
    Input::Keys(&["space"]),
    until!(|m, cx| pending(m, cx, PLAIN)),
    until!(|m, cx| closed(m, cx, PLAIN) && !shown(m, cx, PLAIN) && toast(m, cx, &tr!("tasks-done", title = PLAIN))),
    PICK_UNDO,
    until!(|m, cx| open(m, cx, PLAIN) && shown(m, cx, PLAIN)),
    Input::Press("tasks-nav-5"),
    until!(|m, cx| on_list(m, cx, List::Done) && picked(m, cx).is_some()),
    until!(|m, cx| keep(picked(m, cx).map_or(String::new(), |t| t.title.clone()))),
    Input::Keys(&["space"]),
];

fn reopened(_: &Scene, m: &Mailer, cx: &App) -> bool {
    toast(m, cx, &tr!("tasks-reopened")) && open(m, cx, &kept())
}

fn tab_named(m: &Mailer, cx: &App, key: &str) -> bool {
    m.tab_title(&m.tabs.active(), cx).0 == tr!(key)
}

const NEW: &[Input] = &[
    TASKS,
    IN_TASKS,
    until!(|m, cx| tab_named(m, cx, "frame-section-tasks")),
    Input::Keys(&["ctrl-n"]),
    until!(|m, cx| view(m, cx).draft.is_some() && view(m, cx).title.is_some()),
    Input::Text("Дрилл новая"),
    ENTER,
    until!(|m, cx| task(m, cx, "Дрилл новая").is_some_and(|t| due_day(t, cx) == Some(today(cx))) && shown(m, cx, "Дрилл новая")),
    Input::Keys(&["ctrl-n"]),
    until!(|m, cx| view(m, cx).draft.is_some()),
    Input::Keys(&["escape"]),
    until!(|m, cx| view(m, cx).draft.is_none() && !view(m, cx).tasks.iter().any(|t| t.title.is_empty())),
    QUICK,
    Input::Text("Дрилл строкой"),
    ENTER,
    until!(|m, cx| task(m, cx, "Дрилл строкой").is_some_and(|t| due_day(t, cx) == Some(today(cx)))),
    until!(|m, cx| view(m, cx).status_text() == tr!("tasks-status-mail", n = 0i64) || !view(m, cx).status_text().is_empty()),
    Input::Keys(&["ctrl-2"]),
    until!(|m, cx| tab_named(m, cx, "frame-section-calendar")),
    Input::Keys(&["ctrl-3"]),
    until!(|m, cx| tab_named(m, cx, "frame-section-contacts")),
    Input::Keys(&["ctrl-1"]),
    until!(|m, cx| m.section == Section::Mail && rows_loaded(m.list.read(cx))),
    Input::Keys(&["ctrl-n"]),
];

const DELETE_UNDER_TOASTS: &[Input] = &[
    READY_MAIL,
    CONTACTS,
    IN_CONTACTS,
    Input::Run(|_, m, _, cx| {
        my_contacts(m, cx);
        Ok(())
    }),
    until!(|m, cx| people(m, cx).selected_email().is_some_and(keep)),
    Input::Keys(&["delete"]),
    until!(|_, _| screens::spotted("delete-yes").is_some()),
    Input::Run(|_, m, _, cx| {
        screens::toast_stack(m, cx);
        Ok(())
    }),
    until!(|m, cx| m.toasts.read(cx).on_screen() == 3 && screens::apart("notice-stack", "delete-yes")),
    Input::Press("delete-yes"),
];

fn contact_deleted(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::spotted("delete-yes").is_none()
        && people(m, cx).selected_email().as_deref() != Some(kept().as_str())
        && m.toasts.read(cx).on_screen() >= 3
}

const OTHER_TASK: &str = "Дрилл другой день";

const OTHER_DAY: &[Input] = &[
    TASKS,
    IN_TASKS,
    until!(|m, cx| view(m, cx).selected_task().is_some_and(|t| keep(t.uid.clone()))),
    QUICK,
    Input::Text("Дрилл другой день завтра"),
    ENTER,
    until!(|m, cx| view(m, cx).tasks.iter().any(|t| t.title.starts_with(OTHER_TASK) && due_day(t, cx) == Some(after(today(cx), 1)))),
];

fn kept_selection(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let v = view(m, cx);
    !v.shown.iter().any(|t| t.title.starts_with(OTHER_TASK)) && v.selected_task().is_some_and(|t| t.uid == kept())
}

const FORM_TAB: &[Input] = &[
    READY_MAIL,
    CONTACTS,
    IN_CONTACTS,
    Input::Keys(&["ctrl-shift-c"]),
    until!(|m, cx| people(m, cx).editing()),
    Input::Text("Анна"), // i18n-ok: ввод сценария
    Input::Keys(&["tab"]),
    Input::Text("Мария"), // i18n-ok: ввод сценария
    Input::Keys(&["tab"]),
    Input::Text("Орг"), // i18n-ok: ввод сценария
    Input::Keys(&["tab"]),
    Input::Text("Глава"), // i18n-ok: ввод сценария
    Input::Keys(&["tab"]),
    Input::Text("anna@example.test"),
    Input::Keys(&["shift-tab", "tab", "tab"]),
    Input::Text("+7"),
];

fn form_filled(_: &Scene, m: &Mailer, cx: &App) -> bool {
    people(m, cx).form_text(cx).is_some_and(|f| {
        f.last == "Анна" && f.first == "Мария" && f.organization == "Орг" && f.title == "Глава" && f.emails == "anna@example.test" && f.phones == "+7" // i18n-ok: ввод сценария
    })
}

fn new_letter(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn())
}

const NORTH: &str = "Договор «Север»";

const KEYS: &[Input] = &[
    TASKS,
    IN_TASKS,
    nav!(Nav::Project(NORTH.into())),
    pick!(demo::MAIN),
    Input::Press("sub-2"),
    until!(|m, cx| subtasks(m, cx, demo::MAIN).0 == 3),
    Input::Keys(&["insert"]),
    until!(|m, cx| view(m, cx).due_open),
    Input::Press("due-opt-1"),
    until!(|m, cx| !view(m, cx).due_open && task(m, cx, demo::MAIN).is_some_and(|t| due_day(t, cx) == Some(after(today(cx), 1)))),
    Input::Keys(&["insert"]),
    Input::Press("due-none"),
    until!(|m, cx| task(m, cx, demo::MAIN).is_some_and(|t| t.due.is_none())),
    Input::Keys(&["f2"]),
    until!(|m, cx| view(m, cx).title.is_some()),
    Input::Keys(&["ctrl-a"]),
    Input::Text("Новое имя"),
    ENTER,
    until!(|m, cx| picked_is(m, cx, "Новое имя") && view(m, cx).title.is_none()),
    Input::Keys(&["f2"]),
    until!(|m, cx| view(m, cx).title.is_some()),
    Input::Keys(&["ctrl-a"]),
    Input::Text("Другое"),
    Input::Keys(&["escape"]),
    until!(|m, cx| picked_is(m, cx, "Новое имя") && view(m, cx).title.is_none()),
    Input::Keys(&["delete"]),
    until!(|m, cx| task(m, cx, "Новое имя").is_none() && toast(m, cx, &tr!("tasks-deleted"))),
    PICK_UNDO,
    until!(|m, cx| task(m, cx, "Новое имя").is_some()),
    nav!(Nav::List(List::Today)),
    until!(|m, cx| picked(m, cx).is_some() && keep(picked(m, cx).map_or(String::new(), |t| t.title.clone()))),
    Input::Keys(&["down"]),
    until!(|m, cx| picked(m, cx).is_some_and(|t| t.title != kept())),
    Input::Keys(&["up"]),
    until!(|m, cx| picked_is(m, cx, &kept())),
    Input::Press("card-prio"),
    until!(|m, cx| picked(m, cx).is_some_and(|t| keep(t.important.to_string()))),
    Input::Press("card-prio"),
    until!(|m, cx| picked(m, cx).is_some_and(|t| t.important.to_string() != kept())),
    Input::Keys(&["ctrl-e"]),
    Input::Text("счета"),
];

fn keys_done(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).search.read(cx).value() == "счета" && view(m, cx).shown.len() == 1 && shown(m, cx, PLAIN)
}

fn letter_of(m: &Mailer, cx: &App) -> Option<String> {
    picked(m, cx).and_then(mailtask::letter_key).map(str::to_owned)
}

const FROM_MAIL: &[Input] = &[
    until!(|m, cx| rows_loaded(m.list.read(cx))),
    Input::Click(|m, _, cx| screens::row_place(m, cx, 0), Held::No),
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && keep(r.row.subject.clone()))),
    Input::Keys(&["insert"]),
    Input::Press("lifecycle-pick-2"),
    until!(|m, _| m.lifecycle.picker.is_none()),
    TASKS,
    IN_TASKS,
    Input::Press("tasks-nav-3"),
    until!(|m, cx| on_list(m, cx, List::FromMail) && shown(m, cx, &kept())),
    Input::Run(|_, m, _, cx| {
        let title = kept();
        m.tasks.update(cx, |t, cx| t.select_title(&title, cx));
        Ok(())
    }),
    until!(|m, cx| letter_of(m, cx).is_some() && picked_is(m, cx, &kept())),
    Input::Keys(&["space"]),
    until!(|m, cx| !shown(m, cx, &kept()) && toast(m, cx, &tr!("tasks-done-mail"))),
    PICK_UNDO,
    until!(|m, cx| shown(m, cx, &kept())),
    Input::Run(|_, m, _, cx| {
        let title = kept();
        m.tasks.update(cx, |t, cx| t.select_title(&title, cx));
        Ok(())
    }),
    until!(|m, cx| m.list.read(cx).mailbox.as_ref().is_some_and(|b| keep(b.clone())) && picked(m, cx).is_some()),
    ENTER,
];

fn letter_tab(_: &Scene, m: &Mailer, cx: &App) -> bool {
    matches!(m.tabs.active().kind, TabKind::Mail) && m.list.read(cx).mailbox.as_deref() == Some(kept().as_str())
}

// S6-06.

const DRILL_PROJECT: &str = "Дрилл-проект";

const FIELDS: &[Input] = &[
    TASKS,
    IN_TASKS,
    pick!(PLAIN),
    Input::Press("card-remind"),
    until!(|m, cx| view(m, cx).open == Some(Field::Remind)),
    Input::Press("remind-opt-1"),
    until!(|m, cx| view(m, cx).open.is_none() && picked(m, cx).is_some_and(|t| !t.alerts.is_empty())),
    Input::Press("card-project"),
    until!(|m, cx| view(m, cx).open == Some(Field::Project)),
    Input::Press("proj-new"),
    Input::Text(DRILL_PROJECT),
    ENTER,
    until!(|m, cx| picked(m, cx).is_some_and(|t| t.project.as_deref() == Some(DRILL_PROJECT))),
    Input::Press("card-category"),
    until!(|m, cx| view(m, cx).open == Some(Field::Category)),
    Input::Press("cat-opt-1"),
    until!(|m, cx| view(m, cx).open.is_none()),
    Input::Keys(&["insert"]),
];

fn fields_set(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).due_open && picked(m, cx).is_some_and(|t| t.keywords.iter().any(|k| k.starts_with("pm.cat.")) && t.project.is_some())
}

const SUBS: &[Input] = &[
    TASKS,
    IN_TASKS,
    pick!(PLAIN),
    Input::Press("sub-new"),
    Input::Text("Первая"),
    ENTER,
    until!(|m, cx| subtasks(m, cx, PLAIN) == (0, 1)),
    Input::Text("Вторая"),
    ENTER,
    until!(|m, cx| subtasks(m, cx, PLAIN) == (0, 2)),
    Input::Press("sub-1"),
];

fn subs_done(_: &Scene, m: &Mailer, cx: &App) -> bool {
    subtasks(m, cx, PLAIN) == (1, 2)
}

const NOTE_LINE: &str = "**жирный** и *курсив*";

const NOTES: &[Input] = &[
    TASKS,
    IN_TASKS,
    pick!(PLAIN),
    until!(|_, _| screens::spotted("task-notes").is_some()),
    Input::Press("task-notes"),
    Input::Text(NOTE_LINE),
    until!(|m, cx| view(m, cx).notes.read(cx).value().contains("курсив")),
    pick!(demo::MAIN),
    until!(|m, cx| task(m, cx, PLAIN).is_some_and(|t| t.notes.contains("*курсив*")) && view(m, cx).notes.read(cx).value().contains("SLA")),
    pick!(PLAIN),
];

fn notes_back(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).notes.read(cx).value().as_str() == NOTE_LINE
}

// S6-07.

const REPEAT: &[Input] = &[
    TASKS,
    IN_TASKS,
    pick!(PLAIN),
    Input::Press("card-repeat"),
    until!(|m, cx| view(m, cx).open == Some(Field::Repeat)),
    until!(|_, _| (0..8).all(|i| screens::spotted(&format!("repeat-opt-{i}")).is_some())),
    Input::Press("repeat-opt-3"),
];

fn monthly_set(_: &Scene, m: &Mailer, cx: &App) -> bool {
    picked(m, cx).and_then(recur::rule_of) == Some(Rule::Every { freq: Freq::Monthly(0), interval: 1 }) && view(m, cx).open.is_none()
}

fn copies(m: &Mailer, cx: &App, title: &str) -> usize {
    view(m, cx).tasks.iter().filter(|t| t.title == title && !t.progress.is_closed()).count()
}

const NEXT: &[Input] = &[
    TASKS,
    IN_TASKS,
    until!(|m, cx| task(m, cx, REPEATING).is_some_and(|t| keep(due_day(t, cx).map_or(String::new(), |d| d.to_string())))),
    nav!(Nav::List(List::Upcoming)),
    until!(|m, cx| shown(m, cx, REPEATING) || on_list(m, cx, List::Upcoming)),
    nav!(Nav::List(List::Today)),
    pick!(REPEATING),
    Input::Keys(&["space"]),
    until!(|m, cx| toast_starts(m, cx, &tr!("tasks-done-next", when = "")) && copies(m, cx, REPEATING) == 1),
    until!(|m, cx| !shown(m, cx, REPEATING) || !on_list(m, cx, List::Today)),
    nav!(Nav::List(List::Upcoming)),
    until!(|m, cx| view(m, cx).shown.iter().any(|t| t.title == REPEATING && due_day(t, cx).is_some_and(|d| d.to_string() != kept()))),
    nav!(Nav::List(List::Done)),
    until!(|m, cx| view(m, cx).shown.iter().any(|t| t.title == REPEATING && recur::rule_of(t).is_none())),
    PICK_UNDO,
];

/// «Отменить» — как было: одна открытая задача с прежним сроком и повтором, копии нет.
fn repeat_undone(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let same = view(m, cx)
        .tasks
        .iter()
        .filter(|t| t.title == REPEATING)
        .all(|t| !t.progress.is_closed() && recur::rule_of(t).is_some() && due_day(t, cx).is_some_and(|d| d.to_string() == kept()));
    same && copies(m, cx, REPEATING) == 1
}

const LAST_DAY: &str = "Дрилл: тридцать первое";

/// Задача со сроком 31 марта и повтором «каждый месяц»: год — впереди, чтобы не зависеть от сегодня.
fn with_31st(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    let year = today(cx).year() + 1;
    let day = Date::new(year, 3, 31).map_err(|e| e.to_string())?;
    let mut t = Task::new("drill-31@postman", "demo", LAST_DAY);
    t.due = Some(Moment::new(day.at(10, 0, 0, 0), None));
    recur::set_rule(&mut t, Some(Rule::Every { freq: Freq::Monthly(0), interval: 1 }));
    m.tasks.update(cx, |v, cx| v.on_reply(&Reply::Changed(vec![t]), cx));
    Ok(())
}

const THIRTY_FIRST: &[Input] =
    &[TASKS, IN_TASKS, nav!(Nav::List(List::Upcoming)), until!(|m, cx| shown(m, cx, LAST_DAY)), pick!(LAST_DAY), Input::Keys(&["space"])];

fn thirtieth(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx)
        .tasks
        .iter()
        .any(|t| t.title == LAST_DAY && !t.progress.is_closed() && due_day(t, cx).is_some_and(|d| d.month() == 4 && d.day() == 30))
}

const REMINDED: &str = "Дрилл: напоминание";
/// Настолько раньше сценария наступает срок напоминания: окно не успевает его пропустить.
const REMIND_IN: i64 = 3;

fn with_reminder(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    screens::in_background(true);
    let tz = zone(cx);
    let due = jiff::Timestamp::now().as_second() + 15 * 60 + REMIND_IN;
    let at = jiff::Timestamp::from_second(due).map_err(|e| e.to_string())?.to_zoned(tz);
    let mut t = Task::new("drill-remind@postman", "demo", REMINDED);
    t.due = Some(Moment::new(at.datetime(), None));
    t.alerts = vec![postman_calendar::Alert { trigger: Trigger::Offset { secs: -15 * 60, from: Anchor::Due }, email: false }];
    m.tasks.update(cx, |v, cx| v.on_reply(&Reply::Changed(vec![t]), cx));
    Ok(())
}

const REMIND: &[Input] = &[
    until!(|m, cx| toast(m, cx, &tr!("tasks-remind-toast", title = REMINDED))),
    until!(|_, _| screens::notified().contains(&(tr!("tasks-remind-title"), REMINDED.to_owned()))),
    Input::Press("notice-act"),
];

fn snoozed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).snoozed.iter().any(|(u, at)| u == "drill-remind@postman" && *at > jiff::Timestamp::now().as_second() + 9 * 60)
}

const DEFAULT: &[Input] = &[
    Input::Run(|_, _, _, cx| {
        crate::prefs::update(cx, |p| p.calendar.reminder = 60);
        Ok(())
    }),
    TASKS,
    IN_TASKS,
    pick!(PLAIN),
    Input::Press("card-remind"),
    Input::Press("remind-opt-0"),
];

fn default_first(_: &Scene, m: &Mailer, cx: &App) -> bool {
    picked(m, cx).is_some_and(|t| t.alerts.first().is_some_and(|a| matches!(a.trigger, Trigger::Offset { secs: -3600, .. })))
}

// S6-08.

const BLOCK: &[Input] = &[
    TASKS,
    IN_TASKS,
    pick!(PLAIN),
    until!(|_, _| screens::spotted("block-place").is_some()),
    Input::Keys(&["right"]),
    until!(|m, cx| view(m, cx).slot == 1),
    Input::Keys(&["left"]),
    until!(|m, cx| view(m, cx).slot == 0),
    Input::Press("slot-next"),
    until!(|m, cx| view(m, cx).slot == 1),
    Input::Press("slot-prev"),
    until!(|m, cx| view(m, cx).slot == 0),
    Input::Press("block-place"),
    until!(|m, cx| picked(m, cx).is_some_and(|t| t.block.is_some()) && toast_starts(m, cx, &tr!("tasks-block-placed", when = ""))),
    Input::Press("block-remove"),
];

fn block_removed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    picked(m, cx).is_some_and(|t| t.block.is_none()) && screens::spotted("block-place").is_some()
}

// S6-09, S6-10.

fn status_of(m: &Mailer, cx: &App, title: &str) -> Option<Status> {
    task(m, cx, title).and_then(assign::status)
}

fn demo_toast(key: &str, title: &str) -> String {
    tr!("assign-toast-demo", text = tr!(key, title = title))
}

const STATUSES: &[Input] = &[TASKS, IN_TASKS, nav!(Nav::Project(d::PROJECT.into()))];

fn all_statuses(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let of = |s: Status| view(m, cx).shown.iter().any(|t| assign::status(t) == Some(s));
    [Status::Waiting, Status::Proposed, Status::Accepted, Status::InProgress, Status::Done, Status::Declined].into_iter().all(of)
}

const PROJECT: Input = nav!(Nav::Project(d::PROJECT.into()));
const FORM_GONE: Input = until!(|m, cx| view(m, cx).deleg.edit.is_none());

const RETURN: &[Input] = &[
    TASKS,
    IN_TASKS,
    PROJECT,
    pick!(d::REVIEW),
    Input::Keys(&["insert"]),
    until!(|m, cx| view(m, cx).deleg.edit == Some(Edit::ChangeDue)),
    Input::Keys(&["escape"]),
    FORM_GONE,
    Input::Press("assign-return"),
    until!(|m, cx| view(m, cx).deleg.edit == Some(Edit::Return)),
    Input::Keys(&["escape"]),
    FORM_GONE,
    Input::Press("assign-return"),
    until!(|m, cx| view(m, cx).deleg.edit == Some(Edit::Return)),
    Input::Press("assign-comment"),
    Input::Text("Добавьте выводы"),
    ENTER,
];

fn returned(_: &Scene, m: &Mailer, cx: &App) -> bool {
    status_of(m, cx, d::REVIEW) == Some(Status::InProgress) && toast(m, cx, &demo_toast("assign-toast-returned", d::REVIEW))
}

const ACCEPT: &[Input] = &[TASKS, IN_TASKS, PROJECT, pick!(d::REVIEW), Input::Keys(&["space"])];

fn result_accepted(_: &Scene, m: &Mailer, cx: &App) -> bool {
    status_of(m, cx, d::REVIEW) == Some(Status::Closed) && toast(m, cx, &demo_toast("assign-toast-result-accepted", d::REVIEW))
}

const PROPOSED: &[Input] = &[
    TASKS,
    IN_TASKS,
    PROJECT,
    pick!(PROPOSING),
    until!(|m, cx| task(m, cx, PROPOSING).is_some_and(|t| assign::proposed(t).is_some())),
    Input::Press("assign-agree"),
    until!(|m, cx| status_of(m, cx, PROPOSING) != Some(Status::Proposed) && toast(m, cx, &demo_toast("assign-toast-agreed", PROPOSING))),
    pick!(DECLINED),
    Input::Press("assign-reassign"),
];

fn reassign_form(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).deleg.edit == Some(Edit::Assign { reassign: true }) && screens::spotted("assign-person").is_some()
}

const DELEG_REMIND: &[Input] = &[
    TASKS,
    IN_TASKS,
    PROJECT,
    pick!(WORK),
    Input::Press("assign-remind"),
    until!(|m, cx| toast(m, cx, &demo_toast("assign-toast-reminded", WORK)) && keep(m.toasts.read(cx).stack_history().count().to_string())),
    Input::Press("assign-remind"),
    until!(|_, _| pause(300)),
    until!(|m, cx| m.toasts.read(cx).stack_history().count().to_string() == kept()),
    Input::Press("assign-revoke"),
    until!(|m, cx| status_of(m, cx, WORK).is_none()
        && toast(m, cx, &tr!("assign-toast-demo", text = tr!("assign-toast-taken", list = tr!("tasks-upcoming"))))),
    PICK_UNDO,
];

fn revoke_undone(_: &Scene, m: &Mailer, cx: &App) -> bool {
    status_of(m, cx, WORK) == Some(Status::InProgress)
}

const INCOMING: &[Input] = &[
    TASKS,
    IN_TASKS,
    PROJECT,
    pick!(d::INCOMING),
    Input::Press("assign-propose"),
    until!(|m, cx| view(m, cx).deleg.edit == Some(Edit::Propose)),
    until!(|_, _| screens::spotted("assign-due-1").is_some() && screens::spotted("assign-comment").is_some()),
    Input::Press("assign-cancel"),
    FORM_GONE,
    Input::Press("assign-decline"),
    until!(|m, cx| view(m, cx).deleg.edit == Some(Edit::Decline)),
    Input::Press("assign-cancel"),
    FORM_GONE,
    Input::Press("assign-accept"),
    until!(|m, cx| status_of(m, cx, d::INCOMING) == Some(Status::Accepted)),
    Input::Press("assign-start"),
    until!(|m, cx| status_of(m, cx, d::INCOMING) == Some(Status::InProgress)),
    Input::Press("assign-finish"),
    until!(|m, cx| status_of(m, cx, d::INCOMING) == Some(Status::Done)),
    Input::Press("assign-recall"),
];

fn recalled(_: &Scene, m: &Mailer, cx: &App) -> bool {
    status_of(m, cx, d::INCOMING) == Some(Status::InProgress) && toast(m, cx, &demo_toast("assign-toast-recalled", d::INCOMING))
}

const ASSIGN: &[Input] = &[
    TASKS,
    IN_TASKS,
    pick!(d::TO_ASSIGN),
    Input::Press("task-delegate"),
    until!(|m, cx| matches!(view(m, cx).deleg.edit, Some(Edit::Assign { reassign: false }))),
    until!(|m, _| !m.compose.book.borrow().suggest("ива", 1, 0).is_empty()),
    Input::Press("assign-person"),
    // «ива» в английской раскладке.
    Input::Text("bdf"),
    until!(|m, cx| !view(m, cx).deleg.suggest.is_empty()),
    ENTER,
    until!(|m, cx| view(m, cx).deleg.to.is_some()),
    Input::Press("assign-comment"),
    Input::Text("Подготовьте вопросы"),
    ENTER,
];

fn assigned(_: &Scene, m: &Mailer, cx: &App) -> bool {
    status_of(m, cx, d::TO_ASSIGN) == Some(Status::Waiting) && toast(m, cx, &demo_toast("assign-toast-assigned", d::TO_ASSIGN))
}

// S6-11.

fn deleg_heads(m: &Mailer, cx: &App) -> Vec<Bucket> {
    heads(m, cx).into_iter().filter_map(|g| if let Group::Deleg(b) = g { Some(b) } else { None }).collect()
}

const OPEN_DELEG: Input = Input::Press("tasks-nav-deleg");
const ON_BY_ME: Input = until!(|m, cx| view(m, cx).nav == Nav::Delegations(Tab::ByMe));

const BY_ME: &[Input] = &[
    TASKS,
    IN_TASKS,
    OPEN_DELEG,
    ON_BY_ME,
    until!(|m, cx| view(m, cx).own_only && deleg_heads(m, cx) == [Bucket::Review, Bucket::InWork, Bucket::Waiting] && !shown(m, cx, d::WATCHED)),
    Input::Press("deleg-own"),
];

fn watched_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !view(m, cx).own_only && shown(m, cx, d::WATCHED)
}

const TO_ME: &[Input] = &[TASKS, IN_TASKS, OPEN_DELEG, ON_BY_ME, Input::Press("deleg-tab-to-me")];

fn to_me_groups(_: &Scene, m: &Mailer, cx: &App) -> bool {
    view(m, cx).nav == Nav::Delegations(Tab::ToMe) && deleg_heads(m, cx).first() == Some(&Bucket::New) && deleg_heads(m, cx).contains(&Bucket::InWork)
}

const REMIND_ALL: &[Input] = &[
    TASKS,
    IN_TASKS,
    OPEN_DELEG,
    ON_BY_ME,
    Input::Press("deleg-remind-all"),
    until!(|m, cx| m.toasts.read(cx).latest().is_some_and(|t| keep(t.to_owned()))),
    Input::Press("deleg-remind-all"),
    until!(|m, cx| m.toasts.read(cx).latest().is_some_and(|t| t != kept())),
    pick!(WORK),
    Input::Press("task-revoke"),
];

fn taken_back(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let text = tr!("assign-toast-demo", text = tr!("assign-toast-taken", list = tr!("tasks-upcoming")));
    status_of(m, cx, WORK).is_none() && open(m, cx, WORK) && toast(m, cx, &text)
}

const DELETE: &[Input] = &[
    TASKS,
    IN_TASKS,
    OPEN_DELEG,
    ON_BY_ME,
    pick!(WORK),
    Input::Keys(&["delete"]),
    until!(|m, cx| task(m, cx, WORK).is_none()),
    PICK_UNDO,
    until!(|m, cx| status_of(m, cx, WORK) == Some(Status::InProgress)),
    Input::Press("deleg-tab-to-me"),
    until!(|m, cx| view(m, cx).nav == Nav::Delegations(Tab::ToMe)),
    pick!(d::INCOMING),
    Input::Keys(&["delete"]),
];

fn foreign_kept(_: &Scene, m: &Mailer, cx: &App) -> bool {
    task(m, cx, d::INCOMING).is_some() && toast(m, cx, &tr!("assign-delete-not-mine"))
}

const WAIT: &[Input] = &[
    TASKS,
    IN_TASKS,
    OPEN_DELEG,
    ON_BY_ME,
    until!(|m, cx| {
        let n = postman_core::assign::counts(&view(m, cx).tasks, postman_demo::ME, true).waiting;
        n > 0 && view(m, cx).status_text() == tr!("deleg-status-waiting", n = n as i64)
    }),
    Input::Press("deleg-open-wait"),
];

fn wait_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    let q = &list.query;
    m.section == Section::Mail
        && q.scope == crate::filters::Where::All
        && q.criteria.quick.contains(&postman_core::filters::Quick::Waiting)
        && list.count > 0
}

// S5-26.

fn rules_panel<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a crate::rules::RulesPanel> {
    m.settings.rules_panel.as_ref().map(|p| p.read(cx))
}

const TENDERS: &str = "Тендеры";

/// Чип категории в редакторе правила.
fn tender_chip(m: &Mailer, cx: &App) -> Option<Point<Pixels>> {
    let i = rules_panel(m, cx)?.categories.iter().position(|(_, name, _)| name == TENDERS)?;
    screens::spotted(&format!("rules-category-{i}"))
}

const RULE: &[Input] = &[
    until!(|m, cx| rows_loaded(m.list.read(cx))),
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Rules), window, cx);
        Ok(())
    }),
    until!(|m, cx| screens::settings_on(m, Page::Rules, cx) && rules_panel(m, cx).is_some()),
    Input::Press("rules-add"),
    until!(|m, cx| rules_panel(m, cx).is_some_and(|p| p.editor.is_some())),
    Input::Text("тендер"),
    Input::Press("rules-act-kind-0"),
    Input::Press("rules-kind-5"),
    until!(|m, cx| rules_panel(m, cx).and_then(|p| p.editor.as_ref()).is_some_and(|d| d.first().kind == crate::rules::Kind::Category)),
    Input::Press("rules-editor-save"),
    until!(|m, cx| rules_panel(m, cx).is_some_and(|p| p.error.as_deref() == Some(tr!("rules-bad-category").as_str()))),
    Input::Press("rules-act-category-0"),
    until!(|m, cx| tender_chip(m, cx).is_some()),
    Input::Click(|m, _, cx| tender_chip(m, cx), Held::No),
    until!(|m, cx| rules_panel(m, cx).and_then(|p| p.editor.as_ref()).is_some_and(|d| d.first().category.is_some())),
    Input::Press("rules-editor-save"),
];

/// Демо не сохраняет правила: строка списка — из правила, что ушло на запись (`describe`, та же строка в списке).
fn rule_line(_: &Scene, m: &Mailer, cx: &App) -> bool {
    rules_panel(m, cx).is_some_and(|p| {
        let line = p.pending().and_then(|r| r.last()).map(|r| crate::rules::describe(r, &p.mailboxes, &p.categories));
        line.is_some_and(|t| t.contains(TENDERS)) && p.error.is_none()
    })
}

// S5-28.

fn shared_key(m: &Mailer, cx: &App, name: &str) -> Option<String> {
    m.folders.read(cx).accounts.iter().find(|a| a.name == name).map(|a| a.id.clone())
}

fn shared_inbox(m: &Mailer, cx: &App, name: &str) -> Option<String> {
    let a = shared_key(m, cx, name)?;
    m.folders.read(cx).of(Some(&a)).find(|f| f.role.as_deref() == Some("inbox")).map(|f| f.id.clone())
}

fn own_inbox(m: &Mailer, cx: &App) -> Option<String> {
    crate::live::inbox(&m.folders.read(cx).mailboxes)
}

fn folder_row(id: Option<String>) -> Option<Point<Pixels>> {
    screens::spotted(&format!("mb-{}", id?))
}

const SALES: &str = "sales@corp.example";
const BOSS: &str = "director@corp.example";

fn on_folder(m: &Mailer, cx: &App, id: Option<String>) -> bool {
    let list = m.list.read(cx);
    id.is_some() && list.mailbox == id && rows_loaded(list)
}

fn tip_is(text: &str) -> bool {
    screens::shown_tip() == text
}

const TIP_RESET: Input = Input::Run(|_, _, _, _| {
    screens::forget_tip();
    Ok(())
});

const HINTS: &[Input] = &[
    READY_MAIL,
    until!(|m, cx| shared_key(m, cx, SALES).is_some_and(|a| screens::spotted(&format!("acc-{a}")).is_some())),
    Input::Move(|m, _, cx| screens::spotted(&format!("acc-{}", shared_key(m, cx, SALES)?))),
    until!(|_, _| tip_is(&tr!("shared-branch-hint", rights = tr!("shared-rights-send")))),
    TIP_RESET,
    Input::Move(|m, _, cx| screens::spotted(&format!("acc-{}", shared_key(m, cx, BOSS)?))),
];

fn boss_hint(_: &Scene, _: &Mailer, _: &App) -> bool {
    tip_is(&tr!("shared-branch-hint", rights = tr!("shared-rights-read")))
}

const OPEN_SALES: Input = Input::Click(|m, _, cx| folder_row(shared_inbox(m, cx, SALES)), Held::No);
const IN_SALES: Input = until!(|m, cx| on_folder(m, cx, shared_inbox(m, cx, SALES)));
const SALES_SHOWN: Input = until!(|m, cx| folder_row(shared_inbox(m, cx, SALES)).is_some());

const BUTTON: &[Input] = &[
    READY_MAIL,
    until!(|m, cx| m.bar.read(cx).take.is_none()),
    SALES_SHOWN,
    OPEN_SALES,
    IN_SALES,
    until!(|m, cx| m.bar.read(cx).take == Some(Act::Take) && screens::spotted("bar-take").is_some()),
    Input::Move(|_, _, _| screens::spotted("bar-take")),
    until!(|_, _| screens::shown_tip().contains("Ctrl+J")),
    Input::Click(|m, _, cx| folder_row(own_inbox(m, cx)), Held::No),
];

fn own_no_button(_: &Scene, m: &Mailer, cx: &App) -> bool {
    on_folder(m, cx, own_inbox(m, cx)) && m.bar.read(cx).take.is_none()
}

fn row_label(m: &Mailer, cx: &App, i: usize) -> Option<String> {
    m.list.read(cx).row(i).and_then(|r| crate::shared::assigned(r, cx))
}

fn label_of(who: &str) -> Option<String> {
    Some(tr!("shared-assigned", who = who))
}

const MARKS: &[Input] = &[READY_MAIL, SALES_SHOWN, OPEN_SALES, IN_SALES];

fn marks_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let want = [None, label_of(d::PETROV.0), label_of(&tr!("shared-assigned-me")), None, label_of(d::IVANOVA.0), None];
    want.iter().enumerate().all(|(i, w)| row_label(m, cx, i) == *w)
}

const ROW_0: Input = Input::Click(|m, _, cx| screens::row_place(m, cx, 0), Held::No);
const ROW_1: Input = Input::Click(|m, _, cx| screens::row_place(m, cx, 1), Held::No);
const OPENED: Input = until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.doc.is_some()));

fn opened_row(m: &Mailer, cx: &App, i: usize) -> bool {
    screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && m.list.read(cx).row(i).is_some_and(|x| x.id == r.row.id))
}

const TAKE_KEY: Input = Input::Keys(&["ctrl-j"]);

const TAKE: &[Input] = &[
    READY_MAIL,
    SALES_SHOWN,
    OPEN_SALES,
    IN_SALES,
    ROW_0,
    until!(|m, cx| opened_row(m, cx, 0) && row_label(m, cx, 0).is_none() && screens::spotted("shared-act").is_some()),
    TAKE_KEY,
    until!(|m, cx| toast(m, cx, &tr!("shared-toast-taken")) && row_label(m, cx, 0) == label_of(&tr!("shared-assigned-me"))),
    until!(|m, cx| m.bar.read(cx).take == Some(Act::Release)),
    PICK_UNDO,
];

fn take_undone(_: &Scene, m: &Mailer, cx: &App) -> bool {
    row_label(m, cx, 0).is_none() && m.bar.read(cx).take == Some(Act::Take)
}

// Полоса, метка и кнопка взятого письма (S5-28, шаг 9): вид в обеих темах — по снимку.
const STRIP: &[Input] = &[READY_MAIL, SALES_SHOWN, OPEN_SALES, IN_SALES, ROW_0, until!(|m, cx| opened_row(m, cx, 0)), TAKE_KEY];

fn strip_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    row_label(m, cx, 0) == label_of(&tr!("shared-assigned-me"))
        && m.bar.read(cx).take == Some(Act::Release)
        && screens::spotted("shared-act").is_some()
}

const STEAL: &[Input] = &[
    READY_MAIL,
    SALES_SHOWN,
    OPEN_SALES,
    IN_SALES,
    ROW_1,
    until!(|m, cx| opened_row(m, cx, 1) && m.bar.read(cx).take == Some(Act::Steal) && screens::spotted("shared-act").is_some()),
    Input::Press("shared-act"),
    until!(|m, cx| toast(m, cx, &tr!("shared-toast-stolen", who = d::PETROV.0)) && row_label(m, cx, 1) == label_of(&tr!("shared-assigned-me"))),
    TAKE_KEY,
];

fn released(_: &Scene, m: &Mailer, cx: &App) -> bool {
    toast(m, cx, &tr!("shared-toast-released", mailbox = "sales@")) && row_label(m, cx, 1).is_none()
}

fn reply_view<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a crate::composer::Composer> {
    m.compose.inline_view().map(|c| c.read(cx))
}

fn own_sender(m: &Mailer, cx: &App) -> Option<usize> {
    reply_view(m, cx)?.senders.iter().position(|s| s.address.email.eq_ignore_ascii_case(postman_demo::ME))
}

const REPLY: &[Input] = &[
    READY_MAIL,
    SALES_SHOWN,
    OPEN_SALES,
    IN_SALES,
    Input::Click(|m, _, cx| screens::row_place(m, cx, 4), Held::No),
    until!(|m, cx| opened_row(m, cx, 4)),
    Input::Keys(&["ctrl-r"]),
    until!(|m, cx| reply_view(m, cx).is_some_and(|c| {
        let from = c.model.from().email.clone();
        let note = c.shared.as_ref().and_then(|r| r.note(&from));
        c.drawn() && from == SALES && note == Some(tr!("shared-reply-taken", who = d::IVANOVA.0))
    })),
    until!(|m, cx| reply_view(m, cx).and_then(|c| c.shared.as_ref()).is_some_and(|r| {
        r.hint(SALES) == tr!("shared-from-shared-hint", mailbox = "sales@")
            && r.hint(postman_demo::ME) == tr!("shared-from-own-hint", mailbox = "sales@")
    })),
    Input::Press("from"),
    until!(|m, cx| own_sender(m, cx).is_some_and(|i| screens::spotted(&format!("from-item-{i}")).is_some())),
    Input::Click(|m, _, cx| screens::spotted(&format!("from-item-{}", own_sender(m, cx)?)), Held::No),
];

fn reply_personal(_: &Scene, m: &Mailer, cx: &App) -> bool {
    reply_view(m, cx).is_some_and(|c| {
        let from = c.model.from().email.clone();
        from == postman_demo::ME && c.shared.as_ref().and_then(|r| r.note(&from)) == Some(tr!("shared-reply-personal", mailbox = "sales@"))
    })
}

const READ_ONLY: &[Input] = &[
    READY_MAIL,
    until!(|m, cx| folder_row(shared_inbox(m, cx, BOSS)).is_some()),
    Input::Click(|m, _, cx| folder_row(shared_inbox(m, cx, BOSS)), Held::No),
    until!(|m, cx| on_folder(m, cx, shared_inbox(m, cx, BOSS))),
    until!(|m, cx| m.bar.read(cx).take.is_none()),
    ROW_0,
    OPENED,
    TAKE_KEY,
    until!(|m, cx| toast(m, cx, &tr!("shared-read-only-take"))),
    Input::Click(|m, _, cx| folder_row(own_inbox(m, cx)), Held::No),
    until!(|m, cx| on_folder(m, cx, own_inbox(m, cx))),
    ROW_0,
    until!(|m, cx| opened_row(m, cx, 0)),
    TAKE_KEY,
];

fn own_take_toast(_: &Scene, m: &Mailer, cx: &App) -> bool {
    toast(m, cx, &tr!("shared-take-own"))
}

const REVOCATION: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Security), window, cx);
        Ok(())
    }),
];

fn revocation_hint(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let hint = tr!("settings-revocation-hint");
    screens::settings_on(m, Page::Security, cx) && hint.contains("CRL") && hint.contains("OCSP") && hint.contains("10")
}

// S5-29.

const VACATION: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Vacation), window, cx);
        Ok(())
    }),
    Input::Press("vacation-tab-1"),
    until!(|_, _| screens::spotted("vacation-contacts-only").is_some()),
    Input::Press("vacation-tab-0"),
];

fn vacation_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let shown = ["vacation-enabled", "vacation-cancel", "vacation-save"].iter().all(|s| screens::spotted(s).is_some());
    screens::settings_on(m, Page::Vacation, cx)
        && m.settings.vacation_panel.is_some()
        && shown
        && screens::spotted("vacation-contacts-only").is_none()
}

// S5-30.

fn settings_view<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a crate::settings_tab::SettingsView> {
    m.settings.view.as_ref().map(|v| v.read(cx))
}

fn first_found(m: &Mailer, cx: &App, query: &str) -> Option<Page> {
    settings_view(m, cx).filter(|v| v.query() == query).and_then(|_| crate::settings_tab::found(query, crate::features::get(cx)).first().copied())
}

const ORDER: [Page; 13] = [
    Page::General,
    Page::Accounts,
    Page::Reading,
    Page::Viewers,
    Page::Calendar,
    Page::Categories,
    Page::Quick,
    Page::Security,
    Page::Keys,
    Page::Look,
    Page::Signatures,
    Page::Rules,
    Page::Vacation,
];

const SECTIONS: &[Input] = &[
    READY_MAIL,
    Input::Keys(&["ctrl-,"]),
    until!(|m, cx| settings_view(m, cx).is_some() && m.tabs.is_active(TabKind::Settings)),
    until!(|_, cx| crate::settings_tab::found("", crate::features::get(cx)) == ORDER),
    Input::Press("settings-nav-9"),
    until!(|m, cx| screens::settings_on(m, Page::Look, cx)),
    Input::Press("settings-nav-3"),
    until!(|m, cx| screens::settings_on(m, Page::Viewers, cx)),
    Input::Press("settings-search"),
    Input::Text("пароль"),
    until!(|m, cx| first_found(m, cx, "пароль") == Some(Page::Accounts)),
    ALL,
    Input::Text("шрифт"),
    until!(|m, cx| first_found(m, cx, "шрифт") == Some(Page::Look)),
    ALL,
    Input::Text("mailto"),
];

fn mailto_found(_: &Scene, m: &Mailer, cx: &App) -> bool {
    first_found(m, cx, "mailto").is_some()
}

fn look_prefs(cx: &App) -> postman_core::prefs::Look {
    crate::prefs::get(cx).look.clone()
}

const LOOK: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Look), window, cx);
        Ok(())
    }),
    until!(|_, _| screens::spotted("settings-accent-4").is_some()),
    until!(|_, cx| keep(pets_ui::theme::Color::from_hsla(crate::roles::look(cx).accent()).hex())),
    Input::Press("settings-accent-1"),
    until!(|_, cx| look_prefs(cx).accent != Accent::default() && pets_ui::theme::Color::from_hsla(crate::roles::look(cx).accent()).hex() != kept()),
    until!(|_, cx| look_prefs(cx).font_px() == 14),
    Input::Press("settings-font-inc"),
    until!(|_, cx| look_prefs(cx).font_px() == 15),
    Input::Press("settings-font-dec"),
    until!(|_, cx| look_prefs(cx).font_px() == 14),
    Input::Press("settings-density-20"),
    until!(|_, cx| crate::roles::look(cx).compact()),
    until!(|_, cx| keep(look_prefs(cx).avatars.to_string())),
    // В компактной аватары и превью недоступны: щелчок ничего не меняет.
    Input::Press("settings-avatars"),
    until!(|_, _| pause(250)),
    until!(|_, cx| look_prefs(cx).avatars.to_string() == kept()),
    Input::Press("settings-density-24"),
    until!(|_, cx| !crate::roles::look(cx).compact()),
    Input::Press("settings-avatars"),
    until!(|_, cx| look_prefs(cx).avatars.to_string() != kept()),
    Input::Press("settings-preview"),
    until!(|_, cx| !look_prefs(cx).preview),
    // Низ раздела — за краем окна.
    Input::Wheel(|_, _, _| Some(point(px(800.), px(500.))), 900.),
    Input::Press("settings-keytips-delay-800"),
    until!(|_, cx| look_prefs(cx).keytips_delay == 800),
    Input::Press("settings-keytips"),
    until!(|_, cx| !crate::prefs::get(cx).keytips),
    Input::Hold(Held::Ctrl),
    until!(|_, _| pause(300)),
    until!(|_, cx| !crate::keytips::shown(cx)),
    Input::Hold(Held::No),
];

fn look_done(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::settings_on(m, Page::Look, cx) && !crate::prefs::get(cx).keytips
}

// S8-15, S8-16.

fn folders<'a>(m: &'a Mailer, cx: &'a App) -> &'a crate::folders::Folders {
    m.folders.read(cx)
}

fn local_named(m: &Mailer, cx: &App, name: &str) -> Option<postman_cache::MailboxRow> {
    folders(m, cx).mailboxes.iter().find(|f| crate::local::is_local(&f.id) && f.name == name).cloned()
}

fn local_spot(m: &Mailer, cx: &App, name: &str) -> Option<Point<Pixels>> {
    folder_row(local_named(m, cx, name).map(|f| f.id))
}

/// Дерево всё время у нижнего края: группа «На этом компьютере» — последняя.
fn tree_end(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
    m.folders.update(cx, |f, _| f.screen_keep_end());
}

const fn tree_scene(name: &'static str, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, start, ready).input(input).wait(tree_end).on_request()
}

const GROUP_SHOWN: Input = until!(|_, _| screens::spotted("local-group").is_some());
const LOCAL_GROUP: screens::Place = |_, _, _| screens::spotted("local-group");

const GROUP: &[Input] = &[
    READY_MAIL,
    GROUP_SHOWN,
    Input::Move(LOCAL_GROUP),
    until!(|m, cx| {
        let n: u64 = folders(m, cx).of(Some(crate::local::LOCAL)).map(|f| u64::from(f.total)).sum();
        n > 0 && screens::shown_tip() == tr!("local-group-hint", n = n as i64, count = crate::migration::count_text(n))
    }),
    Input::Click(LOCAL_GROUP, Held::No),
    until!(|m, cx| folders(m, cx).is_collapsed(crate::local::LOCAL) && local_spot(m, cx, ARCHIVE).is_none()),
    Input::Click(LOCAL_GROUP, Held::No),
];

const ARCHIVE: &str = postman_demo::store::ARCHIVE;

fn group_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !folders(m, cx).is_collapsed(crate::local::LOCAL) && local_spot(m, cx, ARCHIVE).is_some()
}

const MENU_SHOWN: Input = until!(|m, cx| folders(m, cx).menu_open());
const FIRST: &str = "Дрилл-папка";
const SECOND: &str = "Дрилл-соседняя";
const INNER: &str = "Дрилл-вложенная";
const RENAMED: &str = "Дрилл-имя";

fn parent_of(m: &Mailer, cx: &App, name: &str) -> Option<Option<String>> {
    local_named(m, cx, name).map(|f| f.parent_id)
}

fn local_id(m: &Mailer, cx: &App, name: &str) -> Option<String> {
    local_named(m, cx, name).map(|f| f.id)
}

const LOCAL_EDIT: &[Input] = &[
    READY_MAIL,
    GROUP_SHOWN,
    Input::Right(LOCAL_GROUP),
    MENU_SHOWN,
    Input::Press("folder-menu-item-0"),
    until!(|m, cx| !folders(m, cx).menu_open()),
    Input::Text(FIRST),
    ENTER,
    until!(|m, cx| parent_of(m, cx, FIRST) == Some(None) && local_spot(m, cx, FIRST).is_some()),
    Input::Right(|m, _, cx| local_spot(m, cx, FIRST)),
    MENU_SHOWN,
    Input::Press("folder-menu-item-0"),
    until!(|m, cx| !folders(m, cx).menu_open()),
    Input::Text(INNER),
    ENTER,
    until!(|m, cx| parent_of(m, cx, INNER) == Some(local_id(m, cx, FIRST)) && local_spot(m, cx, INNER).is_some()),
    Input::Right(|m, _, cx| local_spot(m, cx, INNER)),
    MENU_SHOWN,
    Input::Press("folder-menu-item-1"),
    until!(|m, cx| !folders(m, cx).menu_open()),
    Input::Keys(&["ctrl-a"]),
    Input::Text(RENAMED),
    ENTER,
    until!(|m, cx| local_spot(m, cx, RENAMED).is_some() && local_named(m, cx, INNER).is_none()),
    // Не пустую не удалить.
    Input::Right(|m, _, cx| local_spot(m, cx, ARCHIVE)),
    MENU_SHOWN,
    Input::Press("folder-menu-item-2"),
    until!(|m, cx| toast(m, cx, &tr!("local-delete-not-empty")) && local_named(m, cx, ARCHIVE).is_some()),
    // Пустую — можно.
    Input::Right(|m, _, cx| local_spot(m, cx, RENAMED)),
    MENU_SHOWN,
    Input::Press("folder-menu-item-2"),
    until!(|m, cx| local_named(m, cx, RENAMED).is_none()),
    Input::Right(LOCAL_GROUP),
    MENU_SHOWN,
    Input::Press("folder-menu-item-0"),
    until!(|m, cx| !folders(m, cx).menu_open()),
    Input::Text(SECOND),
    ENTER,
    until!(|m, cx| local_spot(m, cx, SECOND).is_some()),
    Input::Drag(|m, _, cx| local_spot(m, cx, SECOND), |m, _, cx| local_spot(m, cx, FIRST)),
    until!(|m, cx| parent_of(m, cx, SECOND) == Some(local_id(m, cx, FIRST))),
    until!(|m, cx| local_spot(m, cx, SECOND).is_some()),
    Input::Drag(|m, _, cx| local_spot(m, cx, SECOND), LOCAL_GROUP),
];

fn back_to_root(_: &Scene, m: &Mailer, cx: &App) -> bool {
    parent_of(m, cx, SECOND) == Some(None)
}

/// Сцены почты берут первые строки «Входящих», перенос — дальние.
const MOVED_ROW: usize = 6;

/// Письма сцены: первым — тема, дальше — номера писем.
fn moved_letters() -> Vec<postman_core::model::LetterId> {
    let kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    kept.iter().skip(1).filter_map(|n| n.parse().ok()).map(postman_core::model::LetterId).collect()
}

const TO_LOCAL: &[Input] = &[
    READY_MAIL,
    Input::Click(|m, _, cx| screens::row_place(m, cx, MOVED_ROW), Held::No),
    until!(|m, cx| opened_row(m, cx, MOVED_ROW) && keep(screens::reading(m, cx).map_or(String::new(), |r| r.row.subject.clone()))),
    Input::Click(|m, _, cx| screens::row_place(m, cx, MOVED_ROW + 1), Held::Ctrl),
    until!(|m, cx| m.list.read(cx).selection.len() >= 2),
    GROUP_SHOWN,
    until!(|m, cx| local_spot(m, cx, ARCHIVE).is_some()),
    // Демо не хранит письма целиком: выделенным — содержимое, как после загрузки.
    Input::Run(|_, m, _, cx| {
        let keys = m.list.read(cx).selection.ids().to_vec();
        seed(m, &keys)?;
        let mut letters = vec![kept()];
        for key in &keys {
            let id = postman_core::store::letter_of_key(&m.cache, key).map_err(|e| e.to_string())?.ok_or("no letter")?;
            letters.push(id.0.to_string());
        }
        *KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = letters;
        Ok(())
    }),
    Input::Drag(|m, _, cx| screens::row_place(m, cx, MOVED_ROW), |m, _, cx| local_spot(m, cx, ARCHIVE)),
    // S8-15, шаг 6: письмо закрыто, выделение снято.
    until!(|m, cx| m.list.read(cx).selection.is_empty() && screens::reading(m, cx).is_none()),
    until!(|m, cx| {
        let subject = kept();
        !m.list.read(cx).rows.1.iter().any(|r| r.subject == subject)
    }),
    // S8-16, шаг 1: вопроса нет, ход — в строке состояния: задание ждёт «удалено у источника», а сервера у демо нет.
    until!(|m, _| m
        .migration
        .active()
        .is_some_and(|j| { j.mode == postman_core::model::MigrationMode::LocalArchive && j.done == 0 && j.total == moved_letters().len() as u64 })),
    until!(|_, _| screens::spotted("migration-status").is_some()),
    until!(|_, _| pause(500)),
    Input::Run(|_, m, _, _| {
        let job = m.migration.active().ok_or("no active job")?.id;
        let answers: Vec<_> = moved_letters().into_iter().map(|letter| postman_core::sync::Event::Destroyed { job, letter }).collect();
        m.core_events(None, &answers);
        Ok(())
    }),
    until!(|m, cx| {
        let n = moved_letters().len() as u64;
        toast_starts(m, cx, &tr!("migration-done-local", n = n as i64, count = crate::migration::count_text(n))) && m.migration.active().is_none()
    }),
    // S8-15, шаг 5: письмо открывается; тело демо берёт по ключу письма, так что «из базы» здесь не проверяется.
    Input::Click(|m, _, cx| local_spot(m, cx, ARCHIVE), Held::No),
    until!(|m, cx| on_folder(m, cx, local_named(m, cx, ARCHIVE).map(|f| f.id))),
    Input::Click(
        |m, _, cx| {
            let list = m.list.read(cx);
            screens::row_place(m, cx, list.rows.1.iter().position(|r| r.subject == kept())? + list.rows.0)
        },
        Held::No,
    ),
];

fn local_opened(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).is_some_and(|r| r.row.subject == kept() && r.doc.is_some())
}

// S8-32, S8-33б: диалог выбора каталога заменён шагом `Input::Run` (`export_to`), сам диалог здесь не проверяется.

fn choose(m: &Mailer) -> Option<&crate::export::Choose> {
    m.export.choose.as_ref()
}

fn asked(m: &Mailer, what: impl Fn(&What) -> bool) -> bool {
    choose(m).is_some_and(|c| what(&c.what))
}

const INBOX_SHOWN: Input = until!(|m, cx| folder_row(own_inbox(m, cx)).is_some());
const INBOX_MENU: Input = Input::Right(|m, _, cx| folder_row(own_inbox(m, cx)));
/// «Экспорт…» в меню своей папки.
const EXPORT_ITEM: Input = Input::Press("folder-menu-item-2");

const EXPORT_BAR: &[Input] = &[
    READY_MAIL,
    INBOX_SHOWN,
    INBOX_MENU,
    MENU_SHOWN,
    EXPORT_ITEM,
    until!(|m, _| choose(m).is_some_and(|c| c.format == Format::Eml && c.n > 0 && c.subfolders > 0 && c.with_subfolders)),
    Input::Press("export-subfolders"),
    until!(|m, _| choose(m).is_some_and(|c| !c.with_subfolders)),
    Input::Press("export-cancel"),
    until!(|m, _| choose(m).is_none()),
    INBOX_MENU,
    MENU_SHOWN,
    EXPORT_ITEM,
    until!(|m, _| choose(m).is_some()),
    Input::Keys(&["escape"]),
    until!(|m, _| choose(m).is_none()),
    ROW_0,
    until!(|m, cx| opened_row(m, cx, 0)),
    Input::Right(|m, _, cx| screens::row_place(m, cx, 0)),
    until!(|m, _| m.menu.is_some()),
    Input::Press("menu-item-14"),
    until!(|m, _| asked(m, |w| matches!(w, What::Letters(_)))),
    Input::Keys(&["escape"]),
    until!(|m, _| choose(m).is_none()),
    Input::Keys(&["ctrl-k"]),
    until!(|m, cx| m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() > 0)),
    Input::Text("экспорт"), // i18n-ok: ввод сценария
    until!(|m, cx| m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() >= 3)),
    ENTER,
    until!(|m, _| asked(m, |w| matches!(w, What::Letters(_)))),
    Input::Keys(&["escape"]),
    until!(|m, _| choose(m).is_none() && m.palette.is_none()),
    Input::Run(|_, m, _, cx| {
        let local = crate::local::LOCAL.to_owned();
        m.export_ask(What::Account(local), cx);
        Ok(())
    }),
];

fn local_asked(_: &Scene, m: &Mailer, _: &App) -> bool {
    asked(m, |w| matches!(w, What::Account(a) if a == crate::local::LOCAL))
}

const EXPORT_FORMATS: &[Input] = &[
    READY_MAIL,
    INBOX_SHOWN,
    INBOX_MENU,
    MENU_SHOWN,
    EXPORT_ITEM,
    Input::Press("export-format"),
    until!(
        |_, _| (0..4).all(|i| screens::spotted(&format!("export-format-item-{i}")).is_some()) && screens::spotted("export-format-item-4").is_none()
    ),
    Input::Press("export-format-item-3"),
];

fn pst_chosen(_: &Scene, m: &Mailer, _: &App) -> bool {
    choose(m).is_some_and(|c| c.format == Format::Pst && c.menu_at.is_none())
}

/// Каталог сценария — внутри временного `LOCALAPPDATA` прогона.
fn export_dir(name: &str) -> PathBuf {
    let base = postman_core::paths::data_root().unwrap_or_else(std::env::temp_dir);
    base.join("drill-export").join(name)
}

fn files_in(name: &str) -> Vec<String> {
    let names = std::fs::read_dir(export_dir(name)).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect());
    names.unwrap_or_default()
}

fn export_there(m: &mut Mailer, name: &str, cx: &mut Context<Mailer>) -> Result<(), String> {
    let dir = export_dir(name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    m.export_to(dir, cx);
    Ok(())
}

fn export_ended(m: &Mailer, cx: &App) -> bool {
    m.export.runs.is_empty() && m.export.pending.is_empty() && choose(m).is_none() && m.toasts.read(cx).latest().is_some()
}

/// Письмо целиком для папки сценария.
const RAW: &[u8] = b"From: drill@example.test\r\nTo: me@example.test\r\nSubject: Drill\r\n\r\nText\r\n";

/// Демо не хранит письма целиком, а экспорт и перенос работают с такими: письмам — содержимое, как после загрузки с сервера.
fn seed(m: &Mailer, keys: &[String]) -> Result<(), String> {
    for key in keys {
        let id = postman_core::store::letter_of_key(&m.cache, key).map_err(|e| e.to_string())?.ok_or("no letter")?;
        m.cache.put_content(postman_cache::LetterId(id.0), RAW, &[]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Папка «На этом компьютере» демо: два десятка писем, все загружены сценарием.
const SMALL: &str = "Проекты";

const SEED: Input = Input::Run(|_, m, _, cx| {
    let id = local_id(m, cx, SMALL).ok_or("no small folder")?;
    let keys: Vec<String> = crate::local::page(&m.cache, &id, 0, 100).into_iter().map(|r| r.id).collect();
    seed(m, &keys)
});

const SMALL_ASK: Input = Input::Run(|_, m, _, cx| {
    let id = local_id(m, cx, SMALL).ok_or("no small folder")?;
    m.export_ask(What::Folder(id), cx);
    Ok(())
});

const EXPORT_MBOX: &[Input] = &[
    READY_MAIL,
    SMALL_ASK,
    Input::Press("export-format"),
    Input::Press("export-format-item-1"),
    until!(|m, _| choose(m).is_some_and(|c| c.format == Format::Mbox)),
    SEED,
    Input::Run(|_, m, _, cx| export_there(m, "mbox", cx)),
    until!(|m, cx| export_ended(m, cx) && m.export.last == Some(Format::Mbox) && !files_in("mbox").is_empty()),
    // S8-32, шаг 8: прошлый формат — по умолчанию.
    SMALL_ASK,
];

fn format_kept(_: &Scene, m: &Mailer, _: &App) -> bool {
    choose(m).is_some_and(|c| c.format == Format::Mbox)
}

const EXPORT_PST: &[Input] = &[
    READY_MAIL,
    SMALL_ASK,
    Input::Press("export-format"),
    Input::Press("export-format-item-3"),
    until!(|m, _| choose(m).is_some_and(|c| c.format == Format::Pst)),
    SEED,
    Input::Run(|_, m, _, cx| export_there(m, "pst", cx)),
    until!(|m, cx| export_ended(m, cx) && files_in("pst").len() == 1),
    SMALL_ASK,
    Input::Run(|_, m, _, cx| export_there(m, "pst", cx)),
];

fn pst_files(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let mut files = files_in("pst");
    files.sort();
    export_ended(m, cx) && files == [format!("{SMALL} (2).pst"), format!("{SMALL}.pst")]
}

const EXPORT_STOP: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, _, cx| {
        m.export_ask(What::All, cx);
        Ok(())
    }),
    Input::Press("export-format"),
    Input::Press("export-format-item-3"),
    until!(|m, _| choose(m).is_some_and(|c| c.format == Format::Pst)),
    Input::Run(|_, m, _, cx| export_there(m, "stop", cx)),
    until!(|_, _| screens::spotted("export-status").is_some()),
    Input::Press("export-status"),
    Input::Press("export-stop"),
];

fn export_stopped(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.export.runs.is_empty()
        && toast_starts(m, cx, tr!("export-stopped", done = "", total = "").split(':').next().unwrap_or_default())
        && files_in("stop").iter().all(|f| !f.ends_with(".tmp"))
}

// T1-06.

const OWN_GROUP: screens::Place = |_, _, _| screens::spotted("own-account");

const GROUPS: &[Input] = &[
    READY_MAIL,
    // В демо нет своей подписи: логин, как у живого ящика.
    Input::Run(|_, m, _, cx| {
        m.folders.update(cx, |f, cx| {
            f.own = Some(postman_demo::ME.to_owned());
            cx.notify();
        });
        Ok(())
    }),
    until!(|_, _| screens::spotted("own-account").is_some()),
    Input::Click(OWN_GROUP, Held::No),
    // Шаг 2: открытая папка в свёрнутой группе: подпись группы — выбранная.
    until!(|m, cx| {
        let tree = folders(m, cx);
        tree.is_collapsed("") && folder_row(own_inbox(m, cx)).is_none() && tree.current == own_inbox(m, cx) && tree.holds_current(crate::folders::OWN)
    }),
    Input::Click(OWN_GROUP, Held::No),
    until!(|m, cx| !folders(m, cx).is_collapsed("") && folder_row(own_inbox(m, cx)).is_some()),
    until!(|m, cx| shared_key(m, cx, SALES).is_some_and(|a| screens::spotted(&format!("acc-{a}")).is_some())),
    Input::Click(|m, _, cx| screens::spotted(&format!("acc-{}", shared_key(m, cx, SALES)?)), Held::No),
    until!(|m, cx| shared_key(m, cx, SALES).is_some_and(|a| folders(m, cx).is_collapsed(&a)) && folder_row(shared_inbox(m, cx, SALES)).is_none()),
    // Шаг 4: свёрнутая остаётся свёрнутой при смене раздела.
    Input::Keys(&["ctrl-2"]),
    until!(|m, _| m.section == Section::Calendar),
    Input::Keys(&["ctrl-1"]),
    until!(|m, cx| m.section == Section::Mail && shared_key(m, cx, SALES).is_some_and(|a| folders(m, cx).is_collapsed(&a))),
    // Шаг 3: переход к папке разворачивает ветку.
    Input::Run(|_, m, _, cx| {
        let id = shared_inbox(m, cx, SALES).ok_or("no sales inbox")?;
        m.open_mailbox(id, cx);
        Ok(())
    }),
];

fn branch_opened(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let id = shared_inbox(m, cx, SALES);
    shared_key(m, cx, SALES).is_some_and(|a| !folders(m, cx).is_collapsed(&a)) && folders(m, cx).current == id && folder_row(id).is_some()
}

// T1-09.

fn calendar<'a>(m: &'a Mailer, cx: &'a App) -> &'a crate::calendar::CalendarView {
    m.cal.read(cx)
}

/// Смещение сетки: 0 — 00:00 у верхнего края.
fn grid_top(m: &Mailer, cx: &App) -> f32 {
    -f32::from(calendar(m, cx).scroll.offset().y)
}

fn at_work_start(m: &Mailer, cx: &App) -> bool {
    let start = crate::prefs::get(cx).calendar.work_start;
    (grid_top(m, cx) - crate::calendar::minute_y(i64::from(start))).abs() < 1.
}

const EARLY: &str = "Дрилл: ранняя встреча";

/// Событие сегодня в 07:00: до начала рабочего дня.
fn early_event(cx: &App) -> Option<postman_calendar::Event> {
    let mut e = postman_demo::calendar::my_events().into_iter().next()?;
    let zone = zone(cx);
    let start = today(cx).at(7, 0, 0, 0).to_zoned(zone).ok()?.timestamp().as_second();
    e.title = EARLY.to_owned();
    e.span = postman_calendar::Span::new(start, start + 1800);
    Some(e)
}

fn grid_place(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    Some(point(px(500.), px(500.)))
}

fn event_spot(m: &Mailer, cx: &App, title: &str) -> Option<Point<Pixels>> {
    let i = calendar(m, cx).events.iter().position(|e| e.title == title)?;
    screens::spotted(&format!("event-{i}"))
}

const WHOLE_DAY: &[Input] = &[
    READY_MAIL,
    Input::Keys(&["ctrl-2"]),
    until!(|m, _| m.section == Section::Calendar),
    Input::Keys(&["1"]),
    Input::Run(|_, m, _, cx| {
        let event = early_event(cx).ok_or("no demo event")?;
        m.cal.update(cx, |c, cx| {
            c.events.push(event);
            cx.notify();
        });
        Ok(())
    }),
    until!(|m, cx| at_work_start(m, cx)),
    Input::Wheel(grid_place, -3000.),
    until!(|m, cx| grid_top(m, cx) == 0. && event_spot(m, cx, EARLY).is_some()),
    Input::Click(|m, _, cx| event_spot(m, cx, EARLY), Held::No),
    until!(|m, cx| calendar(m, cx).selected()),
    Input::Wheel(grid_place, 3000.),
    until!(|m, cx| grid_top(m, cx) > 400. && (grid_top(m, cx) - f32::from(calendar(m, cx).scroll.max_offset().y)).abs() < 1.),
    Input::Keys(&["home"]),
];

fn back_at_work(_: &Scene, m: &Mailer, cx: &App) -> bool {
    at_work_start(m, cx)
}

// T1-22.

fn sent_id(m: &Mailer, cx: &App) -> Option<String> {
    folders(m, cx).of(None).find(|f| f.role.as_deref() == Some("sent")).map(|f| f.id.clone())
}

const SENT: &[Input] = &[
    READY_MAIL,
    until!(|m, cx| folder_row(sent_id(m, cx)).is_some()),
    Input::Click(|m, _, cx| folder_row(sent_id(m, cx)), Held::No),
    until!(|m, cx| on_folder(m, cx, sent_id(m, cx))),
    ROW_0,
    until!(|m, cx| opened_row(m, cx, 0)),
    Input::Keys(&["down"]),
    until!(|m, cx| opened_row(m, cx, 1)),
    Input::Click(|m, _, cx| screens::row_place(m, cx, 2), Held::Ctrl),
    until!(|m, cx| m.list.read(cx).selection.len() >= 2),
    Input::Press("bulk-flag"),
    until!(|m, cx| m.list.read(cx).rows.1.iter().filter(|r| r.flagged).count() >= 2),
    Input::Press("bulk-move"),
    until!(|m, cx| m.list.read(cx).bulk_menu.is_some()),
    Input::Keys(&["escape"]),
];

fn sent_done(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).bulk_menu.is_none()
}

// T1-23.

fn people<'a>(m: &'a Mailer, cx: &'a App) -> &'a crate::contacts::ContactsView {
    m.contacts.read(cx)
}

const CONTACTS: Input = Input::Keys(&["ctrl-3"]);
const IN_CONTACTS: Input = until!(|m, _| m.section == Section::Contacts && screens::spot_first("contact-").is_some());
const CONTACT_MENU: Input = until!(|m, cx| people(m, cx).menu_open());
/// Строка контакта с номером `n` среди видимых.
fn contact(n: usize) -> Option<Point<Pixels>> {
    screens::spots_numbered("contact-").get(n).map(|b| b.center())
}

const ROW_MENU: &[Input] = &[
    READY_MAIL,
    CONTACTS,
    IN_CONTACTS,
    Input::Click(|_, _, _| contact(1), Held::No),
    until!(|m, cx| people(m, cx).selected_email().is_some_and(keep)),
    Input::Right(|_, _, _| contact(2)),
    CONTACT_MENU,
    until!(|m, cx| people(m, cx).selected_email().as_deref() == Some(kept().as_str())),
    // «Копировать адрес»: тост с адресом строки под курсором, карточка на месте; настоящий буфер Windows не тронут.
    Input::Press("contact-menu-item-2"),
    until!(|m, cx| !people(m, cx).menu_open()
        && screens::clipboard().is_some_and(|a| m.toasts.read(cx).latest() == Some(tr!("contacts-copied", email = a).as_str()))
        && people(m, cx).selected_email().as_deref() == Some(kept().as_str())),
    Input::Right(|_, _, _| contact(2)),
    CONTACT_MENU,
    Input::Press("contact-menu-item-0"),
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn() && !c.read(cx).model.to.is_empty())),
    // Письмо с адресатом при закрытии спрашивает о черновике: просто вернуться к контактам.
    Input::Run(|_, m, _, _| {
        m.tabs.activate_kind(TabKind::Main);
        Ok(())
    }),
    until!(|m, _| m.tabs.is_active(TabKind::Main)),
    until!(|m, _| m.section == Section::Contacts && screens::spot_first("contact-").is_some()),
    Input::Right(|_, _, _| contact(2)),
    CONTACT_MENU,
    Input::Press("contact-menu-item-1"),
];

fn meeting_with_contact(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.active().kind == TabKind::Meeting
}

fn my_contacts(m: &mut Mailer, cx: &mut Context<Mailer>) {
    let mine = postman_demo::contacts::mine().first().and_then(|c| c.emails.first());
    m.contacts.update(cx, |c, cx| {
        c.set_source(crate::contacts::Source::Mine, cx);
        if let Some(email) = mine {
            c.select_email(email, cx);
        }
    });
}

const CONTACT_KEYS: &[Input] = &[
    READY_MAIL,
    CONTACTS,
    IN_CONTACTS,
    Input::Run(|_, m, _, cx| {
        my_contacts(m, cx);
        Ok(())
    }),
    until!(|m, cx| people(m, cx).selected_email().is_some_and(keep)),
    Input::Keys(&["shift-f10"]),
    CONTACT_MENU,
    Input::Keys(&["down", "end", "home", "delete", "f2"]),
    until!(|_, _| pause(250)),
    until!(|m, cx| people(m, cx).menu_open() && !people(m, cx).editing()),
    Input::Keys(&["escape"]),
    until!(|m, cx| !people(m, cx).menu_open()),
    Input::Keys(&["shift-f10"]),
    CONTACT_MENU,
    Input::Press("contact-menu-item-3"),
    until!(|m, cx| people(m, cx).editing()),
    Input::Keys(&["escape"]),
    until!(|m, cx| !people(m, cx).editing()),
    Input::Keys(&["shift-f10"]),
    CONTACT_MENU,
    Input::Press("contact-menu-item-4"),
];

fn delete_asked(_: &Scene, _: &Mailer, _: &App) -> bool {
    screens::spotted("delete-yes").is_some()
}

// Календарь: S5-21, шаги 5, 6, S5-22, шаг 1.

const WEEK: &[Input] = &[
    READY_MAIL,
    Input::Keys(&["ctrl-2"]),
    until!(|m, _| m.section == Section::Calendar),
    Input::Keys(&["3"]),
    Input::Run(|_, m, _, cx| {
        m.cal.update(cx, |c, cx| {
            c.go_to_demo_week(cx);
            c.select_pending(cx);
        });
        Ok(())
    }),
    until!(|m, cx| calendar(m, cx).selected() && screens::spotted("accept").is_some() && screens::spotted("legend-0").is_some()),
    Input::Move(|_, _, _| screens::spotted("legend-0")),
];

fn legend_hint(_: &Scene, _: &Mailer, _: &App) -> bool {
    screens::shown_tip() == tr!("cal-legend-accepted", n = 2i64)
}

const COLORS: &[Input] = &[
    READY_MAIL,
    Input::Keys(&["ctrl-2"]),
    until!(|m, _| m.section == Section::Calendar),
    until!(|m, cx| calendar(m, cx).colleagues.len() >= 2),
    Input::Press("cal-colleague-0"),
    until!(|_, _| (0..5).all(|n| screens::spotted(&format!("cal-color-{n}")).is_some())),
    Input::Press("cal-color-4"),
    until!(|_, cx| !crate::prefs::get(cx).calendar.calendar_colors.is_empty()),
    until!(|_, _| pause(800)),
];

fn color_saved(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let colors = &crate::prefs::get(cx).calendar.calendar_colors;
    m.section == Section::Calendar && !colors.is_empty()
}

const OWNERS: &[Input] = &[
    READY_MAIL,
    Input::Keys(&["ctrl-2"]),
    until!(|m, cx| m.section == Section::Calendar && calendar(m, cx).colleagues.len() >= 2),
    until!(|_, _| screens::spotted("cal-colleague-0").is_some()),
];

fn owners_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let c = calendar(m, cx);
    let l = crate::roles::look(cx);
    let color_of = |name: &str| c.colleagues.iter().find(|o| o.name == name).map(|o| c.calendar_color(&o.key()));
    let (Some(first), Some(second)) = (color_of("Иванова Мария"), color_of("Петров Алексей")) else { return false }; // i18n-ok: имена демо
    let mine = c.calendar_color("");
    let tinted = |e: &postman_calendar::Event| {
        let (_, bg, text) = c.colors(e, &l);
        let cal = l.cat(c.calendar_color(&e.owner.as_ref().map(postman_calendar::Owner::key).unwrap_or_default()));
        bg == cal.bg && text == cal.text
    };
    let striped = |e: &postman_calendar::Event| c.colors(e, &l).0 != l.cat(mine).dot;
    let theirs = c.events.iter().filter(|e| e.owner.is_some()).count();
    mine == postman_core::palette::BLUE
        && first != second
        && first != mine
        && second != mine
        && c.owner_names
        && theirs > 0
        && c.events.iter().all(tinted)
        && c.events.iter().any(|e| e.owner.is_none() && striped(e))
}

/// Второй запуск: выбранный цвет на месте (`POSTMAN_SCREENS_PREFS`).
const COLORS_KEPT: &[Input] = &[READY_MAIL, Input::Keys(&["ctrl-2"])];

// Раздел 24, шаг 9 (без сервера): три шага, «Изменить», «Назад», строка отказа на шаге «Пароль».

fn signin<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a crate::signin::SignIn> {
    m.live.signin.as_ref().map(|p| p.read(cx))
}

fn on_step(m: &Mailer, cx: &App, step: crate::signin::Step) -> bool {
    signin(m, cx).is_some_and(|p| p.step == step)
}

const FIRST_RUN: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, window, cx| {
        m.show_signin(None, window, cx);
        Ok(())
    }),
    until!(|m, cx| on_step(m, cx, Step::Address)),
    until!(|_, _| screens::spotted("signin-next").is_some()),
    Input::Text("drill@mail.example.test"),
    // Вручную (T2-28): без поиска сервера по сети — сценарий не зависит от ответа DNS.
    Input::Press("signin-manual"),
    until!(|m, cx| on_step(m, cx, Step::Server)),
    Input::Press("signin-back"),
    until!(|m, cx| on_step(m, cx, Step::Address)),
    Input::Press("signin-next"),
    until!(|m, cx| on_step(m, cx, Step::Server)),
    until!(|_, _| screens::spotted("signin-store-never").is_some()),
    Input::Press("signin-store-never"),
    until!(|m, cx| signin(m, cx).is_some_and(|p| !p.store)),
    Input::Press("signin-store-windows"),
    until!(|m, cx| signin(m, cx).is_some_and(|p| p.store)),
    Input::Run(|_, m, _, cx| {
        let panel = m.live.signin.clone().ok_or("no sign-in panel")?;
        panel.update(cx, |p, cx| p.rejected("Неверный пароль".to_owned(), cx));
        Ok(())
    }),
];

fn refused(_: &Scene, m: &Mailer, cx: &App) -> bool {
    on_step(m, cx, Step::Server) && signin(m, cx).is_some_and(|p| p.error.is_some())
}

// T1-10, T1-10в: письмо Outlook демо: логотип с размером из HTML и большая картинка без него.

/// Ширина логотипа подписи в HTML письма.
const LOGO_WIDTH: f32 = 180.;
/// Кадр большой картинки: воркер уменьшает её до предела стороны.
const CHART_FRAME: u32 = postman_core::render::MAX_IMAGE_SIDE;

/// Логотип в размере из HTML, большая картинка шире логотипа и не шире колонки.
fn pictures_fit(m: &Mailer, cx: &App) -> Option<f32> {
    let chart = screens::picture_width(CHART_FRAME)?;
    let logo = screens::picture_width(postman_demo::SIGN_LOGO_SIZE.0)?;
    let col = f32::from(m.reading.read(cx).column.get());
    (logo == LOGO_WIDTH && chart > LOGO_WIDTH && chart <= col).then_some(chart)
}

const IMAGES: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, _, cx| screens::select_demo(m, |b| screens::has(b, "chart"), cx)),
    OPENED,
    until!(|m, cx| pictures_fit(m, cx).is_some_and(|w| keep(w.to_string()))),
    // Окно уже: большая картинка — по колонке, логотип тот же.
    Input::Resize(1100., 800.),
    until!(|m, cx| pictures_fit(m, cx).is_some_and(|w| w < kept().parse().unwrap_or(0.))),
    Input::Resize(1280., 800.),
];

fn images_fit(_: &Scene, m: &Mailer, cx: &App) -> bool {
    pictures_fit(m, cx).is_some_and(|w| w == kept().parse().unwrap_or(0.))
}

/// Фокус на элементе, которого нет в окне: так бывает, когда поле с фокусом уходит с экрана.
const LOSE_FOCUS: Input = Input::Run(|_, _, window, cx| {
    let lost = cx.focus_handle();
    window.focus(&lost, cx);
    Ok(())
});

const FOCUS_LOST: &[Input] = &[READY_MAIL, LOSE_FOCUS, TASKS, IN_TASKS, LOSE_FOCUS, Input::Keys(&["ctrl-shift-q"])];

fn meeting_tab(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.active().kind == TabKind::Meeting
}

// Подсказки при наведении: разделы 38 (шаг 2), 41 (шаг 3).

const GRID_TIP: &[Input] = &[
    READY_MAIL,
    Input::Keys(&["ctrl-shift-q"]),
    until!(|m, _| m.tabs.active().kind == TabKind::Meeting),
    until!(|_, _| screens::spotted("assistant").is_some()),
    Input::Move(|_, _, _| screens::spotted("assistant")),
];

fn grid_tip(_: &Scene, _: &Mailer, _: &App) -> bool {
    screens::shown_tip().contains("Enter")
}

const INVITE_TIP: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, _, _, cx| {
        crate::prefs::update(cx, |p| p.key_scheme = KeyScheme::Single);
        Ok(())
    }),
    until!(|m, cx| screens::demo_row(m.list.read(cx), crate::invite_card::scenes::invitation).is_some()),
    Input::Run(|_, m, _, cx| screens::select_demo(m, crate::invite_card::scenes::invitation, cx)),
    until!(|_, _| screens::spotted("invite-accept").is_some()),
    Input::Move(|_, _, _| screens::spotted("invite-accept")),
];

fn invite_tip(_: &Scene, _: &Mailer, _: &App) -> bool {
    screens::shown_tip() == format!("{} (A)", tr!("cal-accept"))
}

// S8-23, T1-13в, S8-27, T1-18: образцы вложений собираются здесь же (`zip`), просмотр — воркером, как у письма.

/// Книга, презентация, документ: минимальный пакет.
fn package(parts: &[(&str, String)]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, text) in parts {
        zip.start_file(*name, options).map_err(|e| e.to_string())?;
        zip.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    zip.finish().map(std::io::Cursor::into_inner).map_err(|e| e.to_string())
}

const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn rels(items: &[(&str, &str, &str)]) -> String {
    let list: String = items.iter().map(|(id, kind, to)| format!(r#"<Relationship Id="{id}" Type="{REL}/{kind}" Target="{to}"/>"#)).collect();
    format!(r#"<?xml version="1.0"?><Relationships xmlns="{RELS}">{list}</Relationships>"#)
}

fn docx(paragraphs: usize) -> Result<Vec<u8>, String> {
    const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let body: String = (0..paragraphs).map(|i| format!("<w:p><w:r><w:t>Абзац {i}</w:t></w:r></w:p>")).collect();
    package(&[
        ("[Content_Types].xml", "<Types/>".to_owned()),
        ("_rels/.rels", rels(&[("rId1", "officeDocument", "word/document.xml")])),
        ("word/document.xml", format!(r#"<w:document xmlns:w="{W}"><w:body>{body}</w:body></w:document>"#)),
    ])
}

/// Проект VBA: для проверки важно имя части, содержимое не исполняется.
fn docm() -> Result<Vec<u8>, String> {
    package(&[("[Content_Types].xml", "<Types/>".to_owned()), ("word/vbaProject.bin", "VBA".to_owned())])
}

fn xlsx() -> Result<Vec<u8>, String> {
    const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let sheet = |name: &str| {
        format!(
            r#"<worksheet xmlns="{S}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>{name}</t></is></c><c r="B1"><v>7</v></c></row></sheetData></worksheet>"#
        )
    };
    let book = format!(
        r#"<workbook xmlns="{S}" xmlns:r="{REL}"><sheets><sheet name="Январь" sheetId="1" r:id="rId1"/><sheet name="Февраль" sheetId="2" r:id="rId2"/></sheets></workbook>"#
    );
    package(&[
        ("[Content_Types].xml", "<Types/>".to_owned()),
        ("_rels/.rels", rels(&[("rId1", "officeDocument", "xl/workbook.xml")])),
        ("xl/workbook.xml", book),
        ("xl/_rels/workbook.xml.rels", rels(&[("rId1", "worksheet", "worksheets/sheet1.xml"), ("rId2", "worksheet", "worksheets/sheet2.xml")])),
        ("xl/worksheets/sheet1.xml", sheet("Итог")),
        ("xl/worksheets/sheet2.xml", sheet("План")),
    ])
}

fn pptx() -> Result<Vec<u8>, String> {
    const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
    const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    let slide = |text: &str| {
        format!(
            r#"<p:sld xmlns:p="{P}" xmlns:a="{A}"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
        )
    };
    let deck = format!(
        r#"<p:presentation xmlns:p="{P}" xmlns:r="{REL}"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>"#
    );
    package(&[
        ("[Content_Types].xml", "<Types/>".to_owned()),
        ("_rels/.rels", rels(&[("rId1", "officeDocument", "ppt/presentation.xml")])),
        ("ppt/presentation.xml", deck),
        ("ppt/_rels/presentation.xml.rels", rels(&[("rId1", "slide", "slides/slide1.xml"), ("rId2", "slide", "slides/slide2.xml")])),
        ("ppt/slides/slide1.xml", slide("Первый слайд")),
        ("ppt/slides/slide2.xml", slide("Второй слайд")),
    ])
}

fn odt() -> Result<Vec<u8>, String> {
    const O: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
    const T: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
    package(&[(
        "content.xml",
        format!(
            r#"<office:document-content xmlns:office="{O}" xmlns:text="{T}"><office:body><office:text><text:p>Абзац</text:p></office:text></office:body></office:document-content>"#
        ),
    )])
}

/// Просмотр вложения `name` с этими байтами: как открытие карточки вложения.
fn show(m: &mut Mailer, name: &str, bytes: Vec<u8>, cx: &mut Context<Mailer>) {
    let viewer = crate::settings_viewers::viewer(name, "");
    m.reading.update(cx, |r, cx| r.screen_preview(name, viewer, bytes, cx));
}

/// Разделов в готовом просмотре файла `name`.
fn sections_of(m: &Mailer, cx: &App, name: &str) -> Option<usize> {
    let p = screens::reading(m, cx)?.preview.as_ref().filter(|p| p.name == name)?;
    match &p.state {
        PvState::Ready(Shown::Sections { sections, .. }) => Some(sections.len()),
        _ => None,
    }
}

const SHEETS: &[Input] = &[
    READY_MAIL,
    ROW_0,
    OPENED,
    Input::Run(|_, m, _, cx| {
        show(m, "книга.xlsx", xlsx()?, cx);
        Ok(())
    }),
    until!(|m, cx| sections_of(m, cx, "книга.xlsx") == Some(2)),
    Input::Run(|_, m, _, cx| {
        show(m, "слайды.pptx", pptx()?, cx);
        Ok(())
    }),
    until!(|m, cx| sections_of(m, cx, "слайды.pptx") == Some(2)),
    Input::Run(|_, m, _, cx| {
        show(m, "список.csv", "Имя;Сумма\nИванова;10\n".as_bytes().to_vec(), cx);
        Ok(())
    }),
    until!(|m, cx| sections_of(m, cx, "список.csv") == Some(1)),
    Input::Run(|_, m, _, cx| {
        show(m, "список.tsv", "Имя\tСумма\nИванова\t10\n".as_bytes().to_vec(), cx);
        Ok(())
    }),
    until!(|m, cx| sections_of(m, cx, "список.tsv") == Some(1)),
    Input::Run(|_, m, _, cx| {
        show(m, "заметка.rtf", br"{\rtf1\ansi Note}".to_vec(), cx);
        Ok(())
    }),
    until!(|m, cx| sections_of(m, cx, "заметка.rtf") == Some(1)),
    Input::Run(|_, m, _, cx| {
        show(m, "заметка.md", "# Заголовок\n\nТекст".as_bytes().to_vec(), cx);
        Ok(())
    }),
    until!(|m, cx| sections_of(m, cx, "заметка.md") == Some(1)),
    Input::Run(|_, m, _, cx| {
        show(m, "заметка.odt", odt()?, cx);
        Ok(())
    }),
];

fn odt_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    sections_of(m, cx, "заметка.odt") == Some(1)
}

const WORD: &[Input] = &[
    READY_MAIL,
    ROW_0,
    OPENED,
    Input::Run(|_, m, _, cx| {
        show(m, "договор.docx", docx(40)?, cx);
        Ok(())
    }),
    until!(|m, cx| sections_of(m, cx, "договор.docx") == Some(1)),
    Input::Keys(&["escape"]),
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.preview.is_none())),
    Input::Run(|_, m, _, cx| {
        show(m, "договор.docm", docm()?, cx);
        Ok(())
    }),
];

/// Файл с макросами идёт в карточку проверки, а не в текст; сама карточка — у вложения письма, сцена `mail-27-zip` и соседние.
fn macro_card(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).and_then(|r| r.preview.as_ref()).is_some_and(|p| {
        p.name == "договор.docm"
            && p.viewer == postman_core::attach::Viewer::Office
            && p.risk == postman_core::attach::Risk::Macros
            && matches!(&p.state, PvState::Ready(Shown::Office(s)) if s.macros)
    })
}

const XLSM: &[Input] = &[
    READY_MAIL,
    Input::Click(|m, _, cx| screens::row_place(m, cx, screens::demo_row(m.list.read(cx), screens::outlook_letter)?), Held::No),
    OPENED,
    until!(|_, _| screens::spotted("att-card-2").is_some()),
    Input::Press("att-card-2"),
];

fn xlsm_card(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).and_then(|r| r.preview.as_ref()).is_some_and(|p| {
        p.name == "Бюджет.xlsm" && p.risk == postman_core::attach::Risk::Macros && matches!(&p.state, PvState::Ready(Shown::Office(s)) if s.macros)
    })
}

const VIEWERS: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Viewers), window, cx);
        Ok(())
    }),
];

fn viewers_page(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::settings_on(m, Page::Viewers, cx) && screens::spotted("settings-viewers-open").is_some()
}

const OUTLOOK: &[Input] = &[
    READY_MAIL,
    Input::Click(|m, _, cx| screens::row_place(m, cx, screens::demo_row(m.list.read(cx), screens::outlook_letter)?), Held::No),
    OPENED,
    until!(|_, _| screens::spotted("att-card-0").is_some()),
    Input::Press("att-card-0"),
];

fn card_opens_preview(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).is_some_and(|r| r.preview.is_some())
}

// S6-09, шаг 7.

fn delegation_letter(b: &postman_cache::BodyRow) -> bool {
    screens::has(b, "todo")
}

const TASK_LETTER: &[Input] = &[
    READY_MAIL,
    until!(|m, cx| screens::demo_row(m.list.read(cx), delegation_letter).is_some()),
    Input::Run(|_, m, _, cx| screens::select_demo(m, delegation_letter, cx)),
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && r.task_mail.is_some())),
    until!(|_, _| screens::spotted("mail-assign-open").is_some() && screens::spotted("att-card-0").is_none()),
    Input::Press("mail-assign-main"),
    until!(|m, cx| status_of(m, cx, d::LETTER) == Some(Status::Accepted)),
    Input::Press("mail-assign-open"),
];

fn letter_task_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.section == Section::Tasks && picked_is(m, cx, d::LETTER)
}

// S8-27, шаг 2: модуль лежит в папке, тест кладёт его до запуска.

const MODULE: &[Input] = &[
    READY_MAIL,
    Input::Run(|_, m, window, cx| {
        m.open_settings(Some(Page::Viewers), window, cx);
        Ok(())
    }),
];

fn module_listed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let module = matches!(crate::settings_viewers::viewer("book.fb2", ""), postman_core::attach::Viewer::Module { .. });
    screens::settings_on(m, Page::Viewers, cx) && module
}
