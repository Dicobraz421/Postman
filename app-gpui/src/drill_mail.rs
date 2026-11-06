//! Сценарии с вводом области «почта и письмо» (`docs/MANUAL-TESTS.md`, карточки T1 и S5-19): имя сцены — `mail-<раздел>-<что>`.
//! Механика — `screens.rs`, запуск — тест `mail_drills` в `app-gpui/tests/manual.rs`. `ready` и `Until` проверяют обещанное шагом
//! по модели; цвет и вид — по снимку.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::*;
use jiff::Zoned;
use pets_i18n::tr;
use pets_ui::theme::Themes;
use postman_cache::{BodyRow, EmailRow};
use postman_core::filters::{Check, Group, Quick as Tab};
use postman_core::lifecycle::Bad;
use postman_core::marks::Kind;
use postman_core::prefs::{Accent, Conversation, KeyScheme, Layout, Pane, Quick as Listed, ThreadOrder};
use postman_core::query::Refine;
use postman_core::send_later::Reject;

use crate::compose::{Model, Purpose, Status, segments};
use crate::mailer::{W_FOLDERS, W_LIST};
use crate::menu::Sub;
use crate::outline::Mark;
use crate::palette::Scope;
use crate::preview::{PvState, Shown};
use crate::screens::{self, Held, Input, Scene, demo_row, outlook_letter, rows_loaded};
use crate::settings_tab::Section as SettingsPage;
use crate::tabs::TabKind;
use crate::{Mailer, Section};

pub(crate) const PART: crate::part::Part = crate::part::Part { name: "drill-mail", scenes: SCENES, reset: Some(reset), ..crate::part::Part::NONE };

/// Место в окне по отметке `screens::spot`.
macro_rules! at {
    ($name:expr) => {
        |_, _, _| screens::spotted($name)
    };
}

/// Шаг со своим условием.
macro_rules! until {
    (|$m:tt, $cx:tt| $done:expr) => {
        Input::Until(|_, $m, $cx| $done)
    };
}

const fn scene(name: &'static str, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, start, ready).input(input).on_request()
}

const fn from(name: &'static str, enter: screens::Enter, ready: screens::Ready, input: &'static [Input]) -> Scene {
    Scene::new(name, enter, ready).input(input).on_request()
}

const SCENES: &[Scene] = &[
    // Раздел 6, шаги 4, 5: щелчок по тосту закрывает его и не выбирает письмо; 🔔 — история, счётчик обнулён; Esc закрывает.
    scene("mail-06-toast", toast_ready, TOAST),
    // Раздел 7, шаги 1, 7: курсор в «Кому»; Ctrl+Enter: «Не отправлено», тост, текст на месте. Раздел 35, шаг 4: Alt+S не отправляет.
    scene("mail-07-send", send_refused, SEND),
    // Раздел 7, шаг 6, внутри окна: ✕ убирает вложение.
    from("mail-07-remove", with_attachment, |_, m, cx| model(m, cx).is_some_and(|c| c.attachments().count() == 0), REMOVE),
    // Раздел 10, шаги 4, 8: кнопки «Ответить», «Ответить всем», «Переслать», «Удалить» — то же, что клавиши, без сервера.
    scene("mail-10-bar", deleted, BAR),
    // Раздел 19, шаги 2, 4: цитаты показать и скрыть; щелчок по письму ленты открывает его, лента остаётся.
    from("mail-19-feed", chain_feed, feed_picked, FEED),
    // Раздел 19, шаг 3: Ctrl+→ / Ctrl+← по письмам беседы; в поле поиска — курсор по словам.
    from("mail-19-nav", chain, word_jump, NAV),
    // Раздел 20, шаг 3: клавиша 7 — тост; в поле поиска цифры — текст.
    scene("mail-20-keys", |_, m, cx| m.search.read(cx).value() == "17" && m.toasts.read(cx).unread() == 1, DIGITS),
    // Раздел 21, шаги 5, 6: вчера — «уже прошло», через 10 дней — «не дольше 7 дн.»; Esc закрывает меню и строку, письмо остаётся.
    scene("mail-21-later", later_closed, LATER),
    // Раздел 22, шаги 5, 6: Ctrl+Shift+Tab; «Почта» не закрывается; «…» и Esc; «Везде | Текущая папка» при поиске.
    scene("mail-22-tabs", scope_folder, TABS),
    // Раздел 23, шаги 1, 2: тема — название вкладки с точкой; две вкладки; Ctrl+W на изменённом — вопрос, «Нет» закрывает.
    scene("mail-23-tab", only_main, TAB),
    // Раздел 23, шаг 3: адрес и «;» — чип; Backspace — чип обратно; «Копия», «Скрытая» — строки.
    scene("mail-23-chips", |_, m, cx| screens::compose_tab(m).is_some_and(|c| c.read(cx).copies_shown() == (true, true)), CHIPS),
    // Раздел 23, шаг 4: Ctrl+B / Ctrl+I / Ctrl+U вокруг выделенного слова; кнопки списков, ссылки, цитаты.
    scene("mail-23-format", |_, m, cx| body_has(m, cx, "> "), FORMAT),
    // Раздел 23, шаг 8: Ctrl+R — ответ над письмом; Esc с текстом — вопрос; «Развернуть» — тот же черновик во вкладке.
    scene("mail-23-reply", |_, m, cx| body_has(m, cx, "ответ") && letters(m) == 1, REPLY), // i18n-ok: ввод сценария
    // Раздел 23, шаг 8: ответ с текстом и выбор другого письма — ответ ушёл во вкладку.
    scene("mail-23-switch", |_, m, _| letters(m) == 1 && m.compose.inline.is_none(), SWITCH),
    // Раздел 25, шаг 5: меню «По дате»; щелчок вне меню закрывает его.
    scene("mail-25-sort", |_, m, cx| m.list.read(cx).sort_open, SORT),
    // Раздел 27, шаг 4: → / ← — соседнее вложение, Esc закрывает; в поле поиска стрелки двигают курсор.
    scene("mail-27-steps", word_stays, PREVIEW),
    // Раздел 27, шаг 10: обрезанный PDF — строка ошибки, письмо и лента работают.
    scene("mail-27-broken", broken_shown, OPEN_OUTLOOK).wait(show_broken),
    // Раздел 27, шаг 11: письмо с картинкой 2400 × 1500 — по ширине области чтения, текст сразу под ней.
    Scene::new("mail-27-chart", start, chart_open).wait(pick_chart).on_request(),
    // Раздел 28, шаг 1: панель сроков, Esc и щелчок мимо закрывают, «В пятницу» — метка.
    scene("mail-28-due", |_, m, cx| !badges(m, cx).is_empty() && m.lifecycle.picker.is_none(), DUE),
    // Раздел 28, шаг 2: ошибки ввода даты, прошлая дата, срок через час; «Убрать срок» снимает.
    scene("mail-28-date", no_badges, DATE),
    // Раздел 28, шаг 3 (без наступления срока): «Напомнить» → «Сегодня позже»: тост, письмо ушло из «Входящих».
    scene("mail-28-remind", letter_left, REMIND),
    // Раздел 28, шаг 4: «Ждать ответа…» у письма — метка; «Не ждать ответа» снимает.
    scene("mail-28-wait", no_badges, WAIT),
    // Раздел 28, шаг 7: «Исчезнет…» → «Через 1 д»; дата в прошлом отклоняется; «Никогда» — метки нет.
    scene("mail-28-expire", no_badges, EXPIRE),
    // Раздел 29, шаг 2: Shift+↓/↑, Ctrl+A, Esc; в поле поиска Ctrl+A выделяет текст.
    scene("mail-29-range", |_, m, cx| m.search.read(cx).value() == "x" && m.list.read(cx).selection.is_empty(), RANGE),
    // Раздел 29, шаг 4: «Прочитано» пачкой, «Отменить», цифра — категория пачкой.
    scene("mail-29-read", |_, m, cx| toast_starts(m, cx, "Категория"), READ), // i18n-ok: ввод сценария
    // Раздел 29, шаг 5: Backspace и Delete над выделением, «Отменить»; в «Удалённых» — «Удалить навсегда N писем?» → «Да».
    scene("mail-29-archive", count_fell, ARCHIVE),
    // Раздел 29, шаг 6: «Срок» на выделении — панель «N писем», метка у всех.
    scene("mail-29-due", selected_due, BULK_DUE),
    // Раздел 29, шаг 8: Ctrl+Shift+V — меню с открытым «Переместить»; выбор папки — письмо там, тост.
    scene("mail-29-move", letter_left, MOVE),
    // Раздел 30, шаг 2: выполненные строки палитры — первыми при пустом запросе.
    scene("mail-30-recent", |_, m, _| m.recent.len() >= 2 && m.palette.is_some(), RECENT),
    // Раздел 30, шаг 4: слово из темы — группа «Письма», Enter открывает письмо.
    scene("mail-30-letter", letter_found, PAL_LETTER),
    // Раздел 30, шаг 4: «Жирный» из палитры во вкладке письма.
    scene("mail-30-bold", |_, m, cx| body_has(m, cx, "**слово**"), PAL_BOLD), // i18n-ok: ввод сценария
    // Раздел 30, шаги 8, 9: синтаксис поиска; Esc — снова папка, подсветки нет.
    scene("mail-30-syntax", search_over, SYNTAX),
    // Раздел 32, шаги 2, 3: F3 и Shift+F3 по кругу, в поле поиска тоже; Esc — строки нет.
    scene("mail-32-hits", search_over, HITS),
    // Раздел 35, шаг 1: стрелки ходят, J, K, Ctrl+Shift+G, Ctrl+Shift+F — ничего; Insert — срок; флаг — кнопкой.
    scene("mail-35-keys", |_, m, cx| m.list.read(cx).rows.1.iter().any(|r| r.flagged), OUTLOOK),
    // Раздел 35, шаг 5 (без фильтров): Ctrl+T, своё письмо у вкладки, Ctrl+W закрывает, «Почта» — нет.
    scene("mail-35-tabs", only_main, MAIL_TAB),
    // Раздел 37, шаг 2: одиночные клавиши в списке.
    from("mail-37-single", single, |_, m, cx| m.search.read(cx).value() == "z", SINGLE_KEYS),
    // Раздел 37, шаг 3: в поле поиска J, R, `/` печатаются.
    from("mail-37-search", single, |_, m, cx| m.search.read(cx).value() == "jr/" && letters(m) == 0, SINGLE_SEARCH),
    // Раздел 37, шаг 5: своя клавиша архива: E не архивирует, Y — да.
    from("mail-37-custom", custom, |_, m, cx| gone(m, cx), CUSTOM),
    // Раздел 39, шаг 2: подсказка «Ответить (Ctrl+R)» при наведении.
    scene("mail-39-hint", |_, _, cx| crate::keytips::tip("mail.reply", cx).contains("Ctrl+R"), HINT),
    // Раздел 39, шаг 4: KeyTips выключены — удержание Ctrl плашек не показывает.
    from("mail-39-off", no_tips, |_, _, cx| !crate::keytips::shown(cx), TIPS_OFF),
    // Раздел 40, шаги 1, 2: живая разметка при наборе.
    scene("mail-40-md", |_, m, cx| body_has(m, cx, "\\*не курсив\\*"), MARKDOWN), // i18n-ok: ввод сценария
    // Раздел 40, шаг 5: ответ в области чтения — то же оформление.
    scene("mail-40-reply", |_, m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.body.contains("**важно**")), MD_REPLY), // i18n-ok: ввод сценария
    // T1-14, T1-15: «Вид беседы» в меню письма: «Как Outlook» перестраивает список, «По умолчанию» возвращает.
    scene("mail-t114-view", |_, m, cx| view_of(m, cx).is_none() && m.list.read(cx).flat_mode(cx), VIEW),
    // T1-16б, шаги 2, 3: при «Сначала новые» Ctrl+→ ведёт к более старому письму; кнопка порядка меняет настройку.
    from("mail-t116-order", newest, |_, _, cx| crate::prefs::get(cx).reading.thread_order == ThreadOrder::OldestFirst, ORDER),
    // Раздел 31, шаги 5, 7: «как Outlook»: → разворачивает, ещё → — к письму, ← обратно; Ctrl+щелчок выбирает всю беседу.
    from("mail-31-outlook", outlook, |_, m, cx| m.list.read(cx).selection.is_empty(), OUTLINE),
    // T1-17, шаг 1: три письма — вкладка, вложения `.eml`, тема пустая.
    scene("mail-t117-many", forwarded_many, FORWARD_MANY),
    // T1-17, шаг 2: одно письмо из меню — «FW: …», одно вложение.
    scene("mail-t117-one", |_, m, cx| model(m, cx).is_some_and(|c| c.subject.starts_with("FW:") && c.attachments().count() == 1), FORWARD_ONE),
    // S5-19, шаги 3, 4: сводка трёх писем — кнопки тонированные (снимок).
    scene("mail-s519-summary", |_, m, cx| m.list.read(cx).selection.len() >= 3, SUMMARY),
    // Раздел 10, шаг 7: полоса прокрутки тянется мышью; папка → другая → назад: позиция та же.
    scene("mail-10-scroll", scroll_kept, SCROLL),
    // Раздел 19, шаг 6: письмо без беседы — кнопки нет; пересылка (FW:) в ленте целиком, ответ — без цитаты.
    from("mail-19-solo", solo, forward_whole, SOLO),
    // Раздел 23, шаг 8: Esc в ответе с текстом, «Да» без сервера: текст на месте; «Развернуть» — тот же черновик во вкладке.
    scene("mail-23-draft", same_draft, DRAFT),
    // Раздел 25, шаг 7: две закладки — письма любой из них; плюс «Непрочитанные» — только подходящие под то и другое.
    scene("mail-25-two", two_tabs, TWO_TABS),
    // Раздел 25, шаг 8: «×» и «…» → Быстрые фильтры; в настройках скрыть «Мне», поднять «Истекают»; фильтр скрытой снят.
    scene("mail-25-hide", tabs_moved, HIDE_TABS),
    // Раздел 25, шаг 9: панель чтения снизу — закладки у края списка; скрыта и письмо открыто — закладок нет.
    scene("mail-25-pane", tabs_gone, PANES),
    // Раздел 27, шаг 5: оглавление архива, два опасных файла, на диск ничего не распаковано.
    scene("mail-27-zip", zip_listed, ZIP),
    // Раздел 27, шаг 8 (без диалога Windows): «Сохранить все» в каталог, повтор — копии «(2)», метка «из Интернета».
    scene("mail-27-save", saved_twice, SAVE_ALL),
    // Раздел 28, шаг 3: срок наступил — письмо во «Входящих», наверху, карточка; «Открыть» — прочитано, «Срочное» ушло.
    from("mail-28-arrive", planner, reminder_done, ARRIVE),
    // Раздел 29, шаг 7, мышь: меню над строкой вне выделения, подменю по наведению, пункт подменю, щелчок мимо.
    scene("mail-29-menu", menu_done, MENU_MOUSE),
    // Раздел 29, шаг 7, клавиатура: Shift+F10, «Меню», стрелки, подменю, Esc; j, k и цифры список не трогают.
    from("mail-29-menu-keys", single, |_, m, cx| m.menu.is_none() && toast_starts(m, cx, "Категория"), MENU_KEYS), // i18n-ok: ввод сценария
    // Раздел 29, шаг 7, выделение: «Выбрано N», ответы серые и пропускаются; вне выделения — меню над одной строкой.
    scene("mail-29-menu-many", |_, m, cx| m.menu.is_none() && m.list.read(cx).selection.len() >= 2, MENU_MANY),
    // Раздел 30, шаг 7: чипы «Уточнить» добавляют условие и снимают его; строка чипов идёт колесом.
    scene("mail-30-refine", chips_scrolled, REFINE),
    // Раздел 31, шаг 3: стопка: строка, шапка, Enter, «Развернуть все», Ctrl+→ / Ctrl+←, «Ответить» у письма.
    from("mail-31-stack", stack, stack_replied, STACK),
    // Раздел 31, шаг 8: общий вид и вид папки из настроек меняют открытую папку; «По умолчанию» в меню снимает свой вид.
    scene("mail-31-settings", |_, m, cx| view_of(m, cx).is_none() && m.list.read(cx).flat_mode(cx), VIEW_SETTINGS),
    // Раздел 32, шаг 5: «Следующее совпадение» — F3; переназначение работает.
    scene("mail-32-keys", |_, m, cx| m.reading.read(cx).hit == 1, HIT_KEYS),
    // Раздел 37, шаг 2: A — ответ всем, F — переслать, E — архив.
    from("mail-37-afe", single, |_, m, cx| gone(m, cx), AFE),
    // T1-04г, шаги 1–3: отступ 16 px на уровень, шеврон, переход из палитры разворачивает родителей, счётчик тот же.
    scene("mail-t104-tree", tree_open, TREE),
    // Решение владельца п. 7, T1-04г шаг 3, T1-06 шаг 3: Ctrl+Y — выбор папки, переход разворачивает родителей; в поле — повтор правки.
    scene("mail-t104-go", went_and_redid, GO_TO),
    // Решение владельца п. 10, раздел 29 шаг 7: меню строки беседы — последнее письмо; «Напомнить» — у него одного.
    scene("mail-29-thread-remind", one_reminded, THREAD_REMIND),
    // Решение владельца п. 10, T1-17 шаг 2: «Переслать вложениями» из меню строки беседы — одно письмо.
    scene(
        "mail-29-thread-forward",
        |_, m, cx| model(m, cx).is_some_and(|c| c.subject.starts_with("FW:") && c.attachments().count() == 1),
        THREAD_FORWARD,
    ),
    // T1-05б, шаги 1, 2: разделители тянутся до минимума; двойной щелчок — ширина макета.
    scene("mail-t105-split", |_, m, cx| m.panes == (W_FOLDERS, W_LIST) && crate::prefs::get(cx).layout == Layout::default(), SPLITS),
    // T1-05б, шаг 3: узкое окно: сначала чтение, затем список, затем папки.
    scene("mail-t105-narrow", |_, m, _| m.panes == (f32::from(Layout::MIN_FOLDERS), f32::from(Layout::MIN_LIST)), NARROW),
    // T1-D, шаг 1 (шаг 3 — в тёмной теме): строка в фокусе и наведение. Цвета проверяет `mail_drills` по снимку.
    scene("mail-t1d-focus", |_, m, cx| m.list.read(cx).focused, FOCUS),
    // T1-D, шаг 1: щелчок в поиск: выделение нейтральное у писем, папок и контактов.
    scene("mail-t1d-blur", no_focus, BLUR),
    // T1-D, шаг 2: у каждого акцента свой тон выделения.
    scene("mail-t1d-accents", |_, _, _| tones().len() == Accent::ALL.len(), ACCENTS),
    // T1-D, шаг 4: тема «Серая» из настроек. Цвета проверяет `mail_drills` по снимку.
    scene("mail-t1d-gray", |_, m, cx| crate::roles::look(cx).gray() && m.tabs.is_active(TabKind::Main), GRAY),
    // Перезапуск (разделы 31.8, T1-04г.2, T1-05б.3, T1-D.4): первый прогон меняет, второй — со своим процессом — проверяет.
    scene("mail-restart-set", |_, _, cx| crate::roles::look(cx).gray(), RESTART_SET),
    scene("mail-restart-check", restarted, RESTART_CHECK),
    // S5-19, шаг 1: событие-приглашение: «Принять» залита, остальные тонированные; наведение — темнее (цвета — по снимку).
    from("mail-s519-invite", invitation, |_, m, _| m.section == Section::Calendar && screens::spotted("accept").is_some(), INVITE),
];

const OPENED: Input = until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.doc.is_some()));
const READY: Input = Input::Until(mail_ready);
const LEFT: Input = Input::Click(row0, Held::No);
const INLINE: Input = until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).drawn()));
const PALETTE_SHOWN: Input = until!(|m, cx| m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() > 0));
const NEW: Input = Input::Keys(&["ctrl-n"]);
const COMPOSE: Input = Input::Until(drawn);

const TOAST: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["7"]),
    until!(|m, cx| m.toasts.read(cx).on_screen() == 1 && keep(m.list.read(cx).selected.map_or(0, |i| i + 1))),
    Input::Click(at!("notice"), Held::No),
    until!(|m, cx| m.toasts.read(cx).on_screen() == 0),
    Input::Click(at!("notifications"), Held::No),
    until!(|m, cx| m.toasts.read(cx).is_history_open() && m.toasts.read(cx).unread() == 0),
    Input::Keys(&["escape"]),
];

fn toast_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !m.toasts.read(cx).is_history_open() && m.list.read(cx).selected.map_or(0, |i| i + 1) == kept()
}

const SEND: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    Input::Text("petr@corp.example;"),
    until!(|m, cx| model(m, cx).is_some_and(|c| segments(&c.to).0.len() == 1)),
    Input::Keys(&["alt-s", "tab"]),
    Input::Text("Тема"),                                                                                   // i18n-ok: ввод сценария
    until!(|m, cx| model(m, cx).is_some_and(|c| c.subject == "Тема" && matches!(c.status, Status::None))), // i18n-ok: ввод сценария
    Input::Keys(&["tab"]),
    Input::Text("текст"),                     // i18n-ok: ввод сценария
    until!(|m, cx| body_has(m, cx, "текст")), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-enter"]),
];

fn send_refused(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let toast = m.toasts.read(cx).latest().is_some_and(|t| t == tr!("needs-server"));
    toast && model(m, cx).is_some_and(|c| matches!(c.status, Status::NoServer(Purpose::Send)) && c.body == "текст") // i18n-ok: ввод сценария
}

fn with_attachment(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    let mut draft = postman_compose::Draft::default();
    draft.attachments.push(crate::composer::scenes::sample_attachment());
    m.open_compose(draft, cx);
    Ok(())
}

const REMOVE: &[Input] = &[COMPOSE, Input::Click(at!("remove-attachment"), Held::No)];

const BAR: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Click(at!("frame-reply"), Held::No),
    until!(|m, cx| replying(m, cx, "RE:")),
    Input::Keys(&["escape"]),
    until!(|m, _| m.compose.last().is_none()),
    Input::Click(at!("frame-reply-all"), Held::No),
    until!(|m, cx| replying(m, cx, "RE:")),
    Input::Keys(&["escape"]),
    until!(|m, _| m.compose.last().is_none()),
    Input::Click(at!("frame-forward"), Held::No),
    until!(|m, cx| replying(m, cx, "FW:")),
    Input::Keys(&["escape"]),
    until!(|m, cx| m.compose.last().is_none() && keep_open(m, cx)),
    Input::Click(at!("frame-delete"), Held::No),
    until!(|m, cx| gone(m, cx)
        && toast_starts(m, cx, &tr!("actions-moved-to-trash"))
        && screens::reading(m, cx).is_some_and(|r| keep(r.row.received_at as usize))),
    Input::Keys(&["down"]),
];

fn replying(m: &Mailer, cx: &App, prefix: &str) -> bool {
    m.compose.last().is_some_and(|c| c.read(cx).drawn() && c.read(cx).model.subject.starts_with(prefix))
}

/// Клавиши списка работают и после щелчка по кнопке панели: ↓ открыл другое письмо.
fn deleted(_: &Scene, m: &Mailer, cx: &App) -> bool {
    gone(m, cx) && screens::reading(m, cx).is_some_and(|r| r.row.received_at as usize != kept())
}

fn chain(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    let row = m.cache.rows_by_ids(&[postman_demo::CHAIN_OPEN.into()]).map_err(|e| e.to_string())?.pop();
    let Some(row) = row else { return Err("no demo conversation in the cache".into()) };
    m.open_letter(row, cx);
    Ok(())
}

fn chain_feed(s: &Scene, m: &mut Mailer, w: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.reading.update(cx, |r, _| r.feed_on = true);
    chain(s, m, w, cx)
}

fn feed_ready(m: &Mailer, cx: &App) -> bool {
    let r = m.reading.read(cx);
    r.feed_on && r.feed.as_ref().is_some_and(|f| f.conv.len() == postman_demo::CHAIN.len() && f.ready())
}

fn on_chain(m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).is_some_and(|r| r.row.id == postman_demo::CHAIN_OPEN)
}

const FEED: &[Input] = &[
    until!(|m, cx| feed_ready(m, cx)),
    Input::Click(at!("thread-quotes"), Held::No),
    until!(|m, cx| m.reading.read(cx).quotes),
    Input::Click(at!("thread-quotes"), Held::No),
    until!(|m, cx| !m.reading.read(cx).quotes),
    Input::Click(at!("feed-msg-0"), Held::No),
];

fn feed_picked(_: &Scene, m: &Mailer, cx: &App) -> bool {
    feed_ready(m, cx) && !on_chain(m, cx)
}

const NAV: &[Input] = &[
    until!(|m, cx| on_chain(m, cx) && m.reading.read(cx).feed.is_some()),
    Input::Keys(&["ctrl-left"]),
    until!(|m, cx| screens::reading(m, cx).is_some() && !on_chain(m, cx)),
    Input::Keys(&["ctrl-right"]),
    until!(|m, cx| on_chain(m, cx)),
    Input::Keys(&["ctrl-e"]),
    Input::Text("два слова"),                                 // i18n-ok: ввод сценария
    until!(|m, cx| m.search.read(cx).value() == "два слова"), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-left"]),
    Input::Text("x"),
];

fn word_jump(_: &Scene, m: &Mailer, cx: &App) -> bool {
    // Письмо не проверяем: поиск по мере набора может уже убрать его из списка.
    m.search.read(cx).value() == "два xслова" // i18n-ok: ввод сценария
}

const DIGITS: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["7"]),
    until!(|m, cx| m.toasts.read(cx).latest().is_some_and(|t| t == tr!("labels-no-category", key = "7"))),
    Input::Keys(&["ctrl-e"]),
    Input::Text("17"),
];

const LATER: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    Input::Text("petr@corp.example;"),
    Input::Click(at!("send-later"), Held::No),
    until!(|m, cx| later_menu(m, cx)),
    Input::Keys(&["escape"]),
    until!(|m, cx| !later_menu(m, cx) && letters(m) == 1),
    Input::Click(at!("send-later"), Held::No),
    until!(|m, cx| later_menu(m, cx)),
    Input::Click(at!("later-choice-99"), Held::No),
    until!(|m, cx| later_row(m, cx).0),
    Input::Keys(&["shift-tab", "ctrl-a"]),
    Input::Typed(|| day(-1)),
    Input::Click(at!("later-schedule"), Held::No),
    until!(|m, cx| later_row(m, cx).1 == Some(Reject::Past)),
    Input::Keys(&["shift-tab", "ctrl-a"]),
    Input::Typed(|| day(10)),
    Input::Click(at!("later-schedule"), Held::No),
    until!(|m, cx| later_row(m, cx).1 == Some(Reject::TooFar)),
    Input::Keys(&["escape"]),
];

fn later_menu(m: &Mailer, cx: &App) -> bool {
    screens::compose_tab(m).is_some_and(|c| c.read(cx).later_menu_open())
}

fn later_row(m: &Mailer, cx: &App) -> (bool, Option<Reject>) {
    screens::compose_tab(m).map_or((false, None), |c| c.read(cx).later_picking())
}

fn later_closed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    letters(m) == 1 && !later_row(m, cx).0 && !later_menu(m, cx)
}

/// Дата через `days` дней, как в поле.
fn day(days: i64) -> String {
    let at = Zoned::now().checked_add(jiff::Span::new().days(days)).unwrap_or_else(|_| Zoned::now());
    at.strftime("%d.%m.%Y").to_string()
}

/// Сегодня через час, как в поле.
fn in_hour() -> String {
    let at = Zoned::now().checked_add(jiff::Span::new().hours(1)).unwrap_or_else(|_| Zoned::now());
    at.strftime("%d.%m.%Y %H:%M").to_string()
}

const TABS: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    NEW,
    until!(|m, _| letters(m) == 2),
    Input::Keys(&["ctrl-shift-tab"]),
    until!(|m, _| letters(m) == 2 && matches!(m.tabs.active().kind, TabKind::Compose(_))),
    Input::Keys(&["ctrl-shift-tab"]),
    until!(|m, _| m.tabs.is_active(TabKind::Main)),
    Input::Keys(&["ctrl-w"]),
    until!(|m, _| letters(m) == 2 && m.tabs.is_active(TabKind::Main)),
    Input::Keys(&["ctrl-shift-tab"]),
    until!(|m, _| matches!(m.tabs.active().kind, TabKind::Compose(_))),
    Input::Keys(&["ctrl-w"]),
    until!(|m, _| letters(m) == 1),
    Input::Keys(&["ctrl-w"]),
    until!(|m, _| letters(m) == 0),
    Input::Click(at!("bar-more"), Held::No),
    until!(|m, cx| m.bar.read(cx).more_open),
    Input::Keys(&["escape"]),
    until!(|m, cx| !m.bar.read(cx).more_open),
    Input::Keys(&["ctrl-e"]),
    Input::Text("квартал"), // i18n-ok: ввод сценария
    until!(|_, _| screens::spotted("scope-folder").is_some()),
    Input::Click(at!("scope-folder"), Held::No),
];

fn scope_folder(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.bar.read(cx).scope == crate::bar::Scope::Folder
}

const TAB: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    Input::Keys(&["tab"]),
    Input::Text("Заголовок"),                                                            // i18n-ok: ввод сценария
    until!(|m, cx| m.tab_title(&m.tabs.active(), cx) == ("Заголовок".to_owned(), true)), // i18n-ok: ввод сценария
    NEW,
    until!(|m, cx| letters(m) == 2 && screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn())),
    Input::Keys(&["ctrl-w"]),
    until!(|m, _| letters(m) == 1 && matches!(m.tabs.active().kind, TabKind::Compose(_))),
    Input::Keys(&["ctrl-w"]),
    until!(|m, cx| model(m, cx).is_some_and(|c| c.confirm_close)),
    Input::Click(at!("close-discard"), Held::No),
];

const CHIPS: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    Input::Text("x@partner.example;"),
    until!(|m, cx| model(m, cx).is_some_and(|c| segments(&c.to).0.len() == 1)),
    Input::Keys(&["backspace"]),
    until!(|m, cx| model(m, cx).is_some_and(|c| segments(&c.to).0.is_empty() && segments(&c.to).1.contains("x@partner.example"))),
    Input::Text(";"),
    until!(|m, cx| model(m, cx).is_some_and(|c| segments(&c.to).0.len() == 1)),
    Input::Click(at!("show-cc"), Held::No),
    until!(|m, cx| screens::compose_tab(m).is_some_and(|c| c.read(cx).copies_shown().0)),
    Input::Click(at!("show-bcc"), Held::No),
];

const WORD: &[&str] = &["shift-left", "shift-left", "shift-left", "shift-left", "shift-left"];

const FORMAT: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    Input::Keys(&["tab", "tab"]),
    Input::Text("слово"),                     // i18n-ok: ввод сценария
    until!(|m, cx| body_has(m, cx, "слово")), // i18n-ok: ввод сценария
    Input::Keys(WORD),
    Input::Keys(&["ctrl-b"]),
    until!(|m, cx| body_has(m, cx, "**слово**")), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-i"]),
    until!(|m, cx| body_has(m, cx, "***слово***")), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-u"]),
    until!(|m, cx| body_has(m, cx, "<u>")),
    Input::Click(at!("fmt-bullets"), Held::No),
    until!(|m, cx| body_has(m, cx, "- ")),
    Input::Click(at!("fmt-link"), Held::No),
    until!(|m, cx| body_has(m, cx, "](")),
    Input::Click(at!("fmt-quote"), Held::No),
];

const REPLY: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["ctrl-r"]),
    INLINE,
    Input::Text("ответ"),                                                                             // i18n-ok: ввод сценария
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.body.contains("ответ"))), // i18n-ok: ввод сценария
    Input::Keys(&["escape"]),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.confirm_close)),
    Input::Keys(&["escape"]),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| !c.read(cx).model.confirm_close)),
    Input::Click(at!("expand"), Held::No),
    COMPOSE,
];

const SWITCH: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["ctrl-r"]),
    INLINE,
    Input::Text("ответ"),                                                                             // i18n-ok: ввод сценария
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.body.contains("ответ"))), // i18n-ok: ввод сценария
    Input::Click(row3, Held::No),
];

const SORT: &[Input] = &[
    READY,
    Input::Click(at!("sort-button"), Held::No),
    until!(|m, cx| m.list.read(cx).sort_open),
    Input::Click(row3, Held::No),
    until!(|m, cx| !m.list.read(cx).sort_open),
    Input::Click(at!("sort-button"), Held::No),
];

const OPEN_OUTLOOK: &[Input] = &[READY, Input::Click(outlook_row, Held::No), OPENED];

const PREVIEW: &[Input] = &[
    READY,
    Input::Click(outlook_row, Held::No),
    OPENED,
    until!(|_, _| screens::spotted("att-card-0").is_some()),
    Input::Click(at!("att-card-0"), Held::No),
    until!(|m, cx| preview_name(m, cx).is_some_and(|n| keep(n.len()))),
    Input::Keys(&["right"]),
    until!(|m, cx| preview_name(m, cx).is_some_and(|n| n.len() != kept())),
    Input::Keys(&["left"]),
    until!(|m, cx| preview_name(m, cx).is_some_and(|n| n.len() == kept())),
    Input::Keys(&["escape"]),
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.preview.is_none())),
    Input::Keys(&["ctrl-e"]),
    Input::Text("аб"), // i18n-ok: ввод сценария
    Input::Keys(&["left"]),
    Input::Text("x"),
];

fn word_stays(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.search.read(cx).value() == "аxб" && screens::reading(m, cx).is_none_or(|r| r.preview.is_none()) // i18n-ok: ввод сценария
}

fn preview_name<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a str> {
    screens::reading(m, cx).and_then(|r| r.preview.as_ref()).map(|p| p.name.as_str())
}

fn show_broken(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
    let open = screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && r.preview.is_none());
    if open {
        m.reading.update(cx, |r, cx| r.screen_preview("broken.pdf", postman_core::attach::Viewer::Pdf, b"%PDF-1.7\n1 0 obj".to_vec(), cx));
    }
}

fn broken_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).and_then(|r| r.preview.as_ref()).is_some_and(|p| matches!(p.state, PvState::Failed(_)))
}

fn pick_chart(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
    if screens::reading(m, cx).is_none() && rows_loaded(m.list.read(cx)) {
        let _ = screens::select_demo(m, |b| screens::has(b, "chart"), cx);
    }
}

fn chart_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::reading(m, cx).is_some_and(|r| r.body.as_ref().is_some_and(|b| screens::has(b, "chart")) && r.doc.is_some())
}

const DUE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["insert"]),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.kind == Kind::Due)),
    Input::Keys(&["escape"]),
    until!(|m, _| m.lifecycle.picker.is_none()),
    Input::Keys(&["insert"]),
    until!(|m, _| m.lifecycle.picker.is_some()),
    Input::Click(row3, Held::No),
    until!(|m, _| m.lifecycle.picker.is_none()),
    LEFT,
    OPENED,
    Input::Keys(&["insert"]),
    until!(|_, _| screens::spotted("lifecycle-pick-2").is_some()),
    Input::Click(at!("lifecycle-pick-2"), Held::No),
];

const DATE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["insert"]),
    until!(|_, _| screens::spotted("lifecycle-pick-date").is_some()),
    Input::Click(at!("lifecycle-pick-date"), Held::No),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.input.is_some())),
    Input::Text("завтра"), // i18n-ok: ввод сценария
    Input::Keys(&["enter"]),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.error == Some(Bad::Format))),
    Input::Keys(&["ctrl-a"]),
    Input::Text("32.10"),
    Input::Keys(&["enter"]),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.error == Some(Bad::Format))),
    Input::Keys(&["ctrl-a"]),
    Input::Text("01.01.2020"),
    Input::Keys(&["enter"]),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.error == Some(Bad::Past))),
    Input::Keys(&["ctrl-a"]),
    Input::Typed(in_hour),
    Input::Keys(&["enter"]),
    until!(|m, cx| m.lifecycle.picker.is_none() && !badges(m, cx).is_empty()),
    Input::Keys(&["insert"]),
    until!(|_, _| screens::spotted("lifecycle-remove").is_some()),
    Input::Click(at!("lifecycle-remove"), Held::No),
];

/// Стрелка вниз до пункта меню письма (порядок — `menu.rs`).
const TO_REMIND: &[&str] = &["down", "down", "down", "down", "down", "down", "down", "down"];
const TO_WAIT: &[&str] = &["down", "down", "down", "down", "down", "down", "down", "down", "down"];
const TO_EXPIRE: &[&str] = &["down", "down", "down", "down", "down", "down", "down", "down", "down", "down"];

const RIGHT: Input = Input::Right(row0);
const MENU: Input = Input::Until(menu_open);
const ENTER: Input = Input::Keys(&["enter"]);

const REMIND: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).letter_total()) && keep_open(m, cx)),
    RIGHT,
    MENU,
    Input::Keys(TO_REMIND),
    ENTER,
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.kind == Kind::Remind)),
    until!(|_, _| screens::spotted("lifecycle-pick-0").is_some()),
    Input::Click(at!("lifecycle-pick-0"), Held::No),
];

const WAIT: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    RIGHT,
    MENU,
    Input::Keys(TO_WAIT),
    ENTER,
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.kind == Kind::Wait)),
    until!(|_, _| screens::spotted("lifecycle-pick-2").is_some()),
    Input::Click(at!("lifecycle-pick-2"), Held::No),
    until!(|m, cx| m.lifecycle.picker.is_none() && !badges(m, cx).is_empty()),
    RIGHT,
    MENU,
    Input::Keys(TO_WAIT),
    ENTER,
    until!(|_, _| screens::spotted("lifecycle-remove").is_some()),
    Input::Click(at!("lifecycle-remove"), Held::No),
];

const EXPIRE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    RIGHT,
    MENU,
    Input::Keys(TO_EXPIRE),
    ENTER,
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.kind == Kind::Expire)),
    until!(|_, _| screens::spotted("lifecycle-pick-date").is_some()),
    Input::Click(at!("lifecycle-pick-date"), Held::No),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.input.is_some())),
    Input::Text("01.01.2020"),
    ENTER,
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.error == Some(Bad::Past))),
    Input::Keys(&["escape"]),
    until!(|m, _| m.lifecycle.picker.is_none()),
    RIGHT,
    MENU,
    Input::Keys(TO_EXPIRE),
    ENTER,
    until!(|_, _| screens::spotted("lifecycle-pick-0").is_some()),
    Input::Click(at!("lifecycle-pick-0"), Held::No),
    until!(|m, cx| m.lifecycle.picker.is_none() && !badges(m, cx).is_empty()),
    RIGHT,
    MENU,
    Input::Keys(TO_EXPIRE),
    ENTER,
    until!(|_, _| screens::spotted("lifecycle-remove").is_some()),
    Input::Click(at!("lifecycle-remove"), Held::No),
];

fn no_badges(_: &Scene, m: &Mailer, cx: &App) -> bool {
    badges(m, cx).is_empty() && m.lifecycle.picker.is_none()
}

const RANGE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["shift-down"]),
    until!(|m, cx| !m.list.read(cx).selection.is_empty() && keep(m.list.read(cx).selection.len())),
    Input::Keys(&["shift-down"]),
    until!(|m, cx| m.list.read(cx).selection.len() > kept()),
    Input::Keys(&["shift-up"]),
    until!(|m, cx| m.list.read(cx).selection.len() == kept()),
    Input::Keys(&["ctrl-a"]),
    until!(|m, cx| m.list.read(cx).selection.len() >= m.list.read(cx).count),
    Input::Keys(&["escape"]),
    until!(|m, cx| m.list.read(cx).selection.is_empty()),
    Input::Keys(&["ctrl-e"]),
    Input::Text("abc"),
    Input::Keys(&["ctrl-a"]),
    Input::Text("x"),
];

const CTRL_ROW1: Input = Input::Click(row1, Held::Ctrl);
const TWO_PICKED: Input = until!(|m, cx| m.list.read(cx).selection.len() >= 2);

const READ: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    CTRL_ROW1,
    TWO_PICKED,
    Input::Keys(&["ctrl-u"]),
    until!(|m, cx| toast_starts(m, cx, "Не прочитано")), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-q"]),
    until!(|m, cx| toast_starts(m, cx, "Прочитано")), // i18n-ok: ввод сценария
    Input::Click(at!("notice-act"), Held::No),
    until!(|m, cx| m.toasts.read(cx).latest().is_some_and(|t| t == tr!("bulk-undone"))),
    Input::Keys(&["1"]),
];

const ARCHIVE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    CTRL_ROW1,
    TWO_PICKED,
    until!(|m, cx| keep(m.list.read(cx).count)),
    Input::Keys(&["backspace"]),
    until!(|m, cx| m.list.read(cx).count < kept() && m.list.read(cx).selection.is_empty()),
    until!(|_, _| screens::spotted("notice-act").is_some()),
    Input::Click(at!("notice-act"), Held::No),
    until!(|m, cx| m.list.read(cx).count == kept()),
    LEFT,
    CTRL_ROW1,
    TWO_PICKED,
    Input::Keys(&["delete"]),
    until!(|m, cx| m.list.read(cx).count < kept()),
    Input::Keys(&["ctrl-k"]),
    Input::Text("Удалённые"), // i18n-ok: ввод сценария
    PALETTE_SHOWN,
    ENTER,
    until!(|m, cx| m.palette.is_none() && m.list.read(cx).mailbox.as_deref() != Some("inbox") && rows_loaded(m.list.read(cx))),
    LEFT,
    CTRL_ROW1,
    TWO_PICKED,
    until!(|m, cx| keep(m.list.read(cx).count)),
    Input::Keys(&["delete"]),
    until!(|m, cx| m.list.read(cx).confirm_many.is_some()),
    Input::Click(at!("purge-yes"), Held::No),
];

const BULK_DUE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    CTRL_ROW1,
    TWO_PICKED,
    Input::Keys(&["insert"]),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| !p.rest.is_empty())),
    until!(|_, _| screens::spotted("lifecycle-pick-1").is_some()),
    Input::Click(at!("lifecycle-pick-1"), Held::No),
];

fn selected_due(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    let now = Zoned::now();
    let marked = list.rows.1.iter().filter(|r| list.selection.contains(&r.id) && !crate::lifecycle::row_badges(r, &now).is_empty()).count();
    m.lifecycle.picker.is_none() && marked >= 2
}

const MOVE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).letter_total()) && keep_open(m, cx)),
    Input::Keys(&["ctrl-shift-v"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Move))),
    ENTER,
];

fn count_fell(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).count < kept()
}

/// Письмо ушло из папки, ровно одно. Строк может остаться столько же: меню берёт последнее письмо беседы (решение владельца
/// 2026-11-06, п. 10), а её остальные письма остаются строкой.
fn letter_left(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).letter_total() + 1 == kept() && gone(m, cx)
}

const RECENT: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["ctrl-k"]),
    Input::Text("Ответить всем"), // i18n-ok: ввод сценария
    PALETTE_SHOWN,
    ENTER,
    until!(|m, cx| m.palette.is_none() && m.compose.inline_view().is_some_and(|c| c.read(cx).drawn())),
    Input::Keys(&["escape"]),
    until!(|m, _| m.compose.inline.is_none()),
    Input::Keys(&["ctrl-k"]),
    Input::Text("Архив"), // i18n-ok: ввод сценария
    PALETTE_SHOWN,
    ENTER,
    until!(|m, _| m.palette.is_none()),
    Input::Keys(&["ctrl-k"]),
    PALETTE_SHOWN,
];

const PAL_LETTER: &[Input] = &[
    READY,
    Input::Keys(&["ctrl-k"]),
    Input::Text("квартал"), // i18n-ok: ввод сценария
    until!(|m, cx| m.palette.as_ref().is_some_and(|p| p.view.read(cx).has_letters())),
    // С первой строки вверх — на последнюю: письма — последняя группа, папки стоят выше.
    Input::Keys(&["up", "enter"]),
    OPENED,
];

fn letter_found(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.is_none() && screens::reading(m, cx).is_some_and(|r| r.row.subject.contains("квартал")) // i18n-ok: ввод сценария
}

const PAL_BOLD: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    Input::Keys(&["tab", "tab"]),
    Input::Text("слово"),                     // i18n-ok: ввод сценария
    until!(|m, cx| body_has(m, cx, "слово")), // i18n-ok: ввод сценария
    Input::Keys(WORD),
    Input::Keys(&["ctrl-k"]),
    Input::Text("Жирный"), // i18n-ok: ввод сценария
    PALETTE_SHOWN,
    ENTER,
];

const SYNTAX: &[Input] = &[
    READY,
    Input::Keys(&["ctrl-e"]),
    Input::Text("is:unread subject:отчёт"), // i18n-ok: ввод сценария
    ENTER,
    until!(|m, cx| found(m, cx) && m.list.read(cx).rows.1.iter().all(|r| !r.seen && r.subject.to_lowercase().contains("отчёт"))), // i18n-ok: ввод сценария
    Input::Keys(&["ctrl-a"]),
    Input::Text("before:2020-01-01"),
    ENTER,
    until!(|m, cx| m.find.text.starts_with("before") && !m.list.read(cx).computing && m.list.read(cx).count == 0),
    Input::Keys(&["escape"]),
];

/// Поиск дал строки.
fn found(m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    m.find.active() && list.search_ids.is_some() && !list.computing && rows_loaded(list)
}

fn search_over(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !m.find.active() && m.reading.read(cx).marks.is_none() && m.list.read(cx).search_ids.is_none()
}

const HITS: &[Input] = &[
    READY,
    Input::Keys(&["ctrl-e"]),
    Input::Text("квартал"), // i18n-ok: ввод сценария
    until!(|m, cx| found(m, cx)),
    Input::Click(row1, Held::No),
    until!(|m, cx| m.reading.read(cx).hits.get() > 1 && m.reading.read(cx).hit == 0),
    Input::Keys(&["f3"]),
    until!(|m, cx| m.reading.read(cx).hit == 1),
    Input::Keys(&["shift-f3"]),
    until!(|m, cx| m.reading.read(cx).hit == 0),
    Input::Keys(&["shift-f3"]),
    until!(|m, cx| m.reading.read(cx).hit + 1 == m.reading.read(cx).hits.get()),
    Input::Keys(&["f3"]),
    until!(|m, cx| m.reading.read(cx).hit == 0),
    Input::Keys(&["ctrl-e", "f3"]),
    until!(|m, cx| m.reading.read(cx).hit == 1),
    Input::Keys(&["ctrl-e", "escape"]),
];

const OUTLOOK: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).selected.unwrap_or(0))),
    Input::Keys(&["j", "k", "ctrl-shift-g", "ctrl-shift-f"]),
    Input::Until(|_, _, _| pause(300)),
    until!(|m, cx| m.list.read(cx).selected == Some(kept()) && m.lifecycle.picker.is_none() && !m.filters.read(cx).panel),
    Input::Keys(&["down"]),
    until!(|m, cx| m.list.read(cx).selected == Some(kept() + 1)),
    Input::Keys(&["up"]),
    until!(|m, cx| m.list.read(cx).selected == Some(kept())),
    Input::Keys(&["insert"]),
    until!(|m, _| m.lifecycle.picker.is_some()),
    Input::Keys(&["escape"]),
    until!(|m, _| m.lifecycle.picker.is_none()),
    Input::Click(at!("frame-flag"), Held::No),
];

const MAIL_TAB: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).selected.unwrap_or(0))),
    Input::Keys(&["ctrl-t"]),
    until!(|m, _| m.tabs.is_active(TabKind::Mail)),
    Input::Click(row3, Held::No),
    until!(|m, cx| m.list.read(cx).selected.is_some_and(|i| i != kept())),
    Input::Keys(&["ctrl-tab"]),
    until!(|m, cx| m.tabs.is_active(TabKind::Main) && m.list.read(cx).selected == Some(kept())),
    Input::Keys(&["ctrl-tab"]),
    until!(|m, cx| m.tabs.is_active(TabKind::Mail) && m.list.read(cx).selected.is_some_and(|i| i != kept())),
    Input::Keys(&["ctrl-t"]),
    until!(|m, _| m.tabs.iter().count() == 3),
    Input::Keys(&["ctrl-w"]),
    until!(|m, _| m.tabs.iter().count() == 2 && m.tabs.is_active(TabKind::Mail)),
    Input::Keys(&["ctrl-w"]),
    until!(|m, _| m.tabs.iter().count() == 1),
    Input::Keys(&["ctrl-w"]),
];

fn single(_: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    crate::prefs::update(cx, |p| p.key_scheme = KeyScheme::Single);
    Ok(())
}

const SINGLE_KEYS: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).selected.unwrap_or(0))),
    Input::Keys(&["j"]),
    until!(|m, cx| m.list.read(cx).selected == Some(kept() + 1)),
    Input::Keys(&["k"]),
    until!(|m, cx| m.list.read(cx).selected == Some(kept())),
    Input::Keys(&["u"]),
    until!(|m, cx| m.list.read(cx).row(kept()).is_some_and(|r| !r.seen)),
    Input::Keys(&["q"]),
    until!(|m, cx| m.list.read(cx).row(kept()).is_some_and(|r| r.seen)),
    Input::Keys(&["r"]),
    INLINE,
    Input::Keys(&["escape"]),
    until!(|m, _| m.compose.inline.is_none()),
    Input::Keys(&["c"]),
    COMPOSE,
    Input::Keys(&["escape"]),
    until!(|m, _| letters(m) == 0),
    Input::Keys(&["v"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Move))),
    Input::Keys(&["escape"]),
    until!(|m, _| m.menu.is_none()),
    Input::Keys(&["/"]),
    Input::Until(|_, _, _| pause(200)),
    Input::Text("z"),
];

const SINGLE_SEARCH: &[Input] = &[READY, Input::Keys(&["ctrl-e"]), Input::Text("jr/")];

fn custom(_: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    crate::prefs::update(cx, |p| {
        p.key_scheme = KeyScheme::Single;
        p.keys_single.insert("mail.archive".into(), vec!["y".into()]);
    });
    Ok(())
}

const CUSTOM: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).count)),
    Input::Keys(&["e"]),
    Input::Until(|_, _, _| pause(300)),
    until!(|m, cx| m.list.read(cx).count == kept() && keep_open(m, cx)),
    Input::Keys(&["y"]),
];

const HINT: &[Input] = &[READY, Input::Move(at!("frame-reply")), Input::Until(|_, _, _| pause(900))];

fn no_tips(_: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    crate::prefs::update(cx, |p| p.keytips = false);
    Ok(())
}

const TIPS_OFF: &[Input] =
    &[READY, Input::Hold(Held::Ctrl), Input::Until(|_, _, _| pause(800)), until!(|_, cx| !crate::keytips::shown(cx)), Input::Hold(Held::No)];

const MARKDOWN: &[Input] = &[
    READY,
    NEW,
    COMPOSE,
    Input::Keys(&["tab", "tab"]),
    Input::Text("## Итоги"), // i18n-ok: ввод сценария
    ENTER,
    Input::Text("**важно** *курсив* <u>подчёркнуто</u> ~~старое~~ `код`"), // i18n-ok: ввод сценария
    ENTER,
    Input::Text("[отчёт](https://corp.example)"), // i18n-ok: ввод сценария
    ENTER,
    Input::Text("> цитата"), // i18n-ok: ввод сценария
    Input::Keys(&["enter", "enter"]),
    Input::Text("**слово \\*не курсив\\*"), // i18n-ok: ввод сценария
];

const MD_REPLY: &[Input] = &[READY, LEFT, OPENED, Input::Keys(&["ctrl-r"]), INLINE, Input::Text("**важно** и *курсив*")]; // i18n-ok: ввод сценария

/// Открыть подменю «Вид беседы»: снизу вверх, мимо «Сообщить о фишинге».
const TO_VIEW: &[&str] = &["up", "up", "right"];

const VIEW: &[Input] = &[
    READY,
    RIGHT,
    MENU,
    Input::Keys(TO_VIEW),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::View))),
    Input::Keys(&["down", "down", "enter"]),
    until!(|m, cx| view_of(m, cx) == Some(Conversation::Outlook) && !m.list.read(cx).flat_mode(cx) && m.menu.is_none()),
    RIGHT,
    MENU,
    Input::Keys(TO_VIEW),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::View))),
    Input::Keys(&["down", "down", "down", "enter"]),
];

fn view_of(m: &Mailer, cx: &App) -> Option<Conversation> {
    let mailbox = m.list.read(cx).mailbox.clone()?;
    crate::prefs::get(cx).reading.folders.get(&mailbox).copied()
}

fn newest(s: &Scene, m: &mut Mailer, w: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    crate::prefs::update(cx, |p| p.reading.thread_order = ThreadOrder::NewestFirst);
    chain_feed(s, m, w, cx)
}

const ORDER: &[Input] = &[
    until!(|m, cx| feed_ready(m, cx) && screens::reading(m, cx).is_some_and(|r| keep(r.row.received_at as usize))),
    Input::Keys(&["ctrl-right"]),
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| (r.row.received_at as usize) < kept())),
    Input::Click(at!("thread-order"), Held::No),
];

fn outlook(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    crate::prefs::update(cx, |p| p.reading.set_folder("inbox", Some(Conversation::Outlook)));
    // Как выбор вида в меню: папка перестраивается.
    m.open_mailbox("inbox".into(), cx);
    Ok(())
}

fn chain_open(m: &Mailer, cx: &App) -> bool {
    !m.list.read(cx).open_threads().is_empty()
}

const OUTLINE: &[Input] = &[
    READY,
    until!(|m, cx| m.list.read(cx).outline.is_some() && !m.list.read(cx).computing),
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).selected.unwrap_or(0))),
    Input::Keys(&["right"]),
    until!(|m, cx| chain_open(m, cx)),
    Input::Keys(&["right"]),
    until!(|m, cx| m.list.read(cx).selected == Some(kept() + 1)),
    Input::Keys(&["left"]),
    until!(|m, cx| m.list.read(cx).selected == Some(kept())),
    Input::Keys(&["left"]),
    until!(|m, cx| !chain_open(m, cx)),
    ENTER,
    until!(|m, cx| chain_open(m, cx)),
    ENTER,
    until!(|m, cx| !chain_open(m, cx)),
    Input::Click(row0, Held::Ctrl),
    until!(|m, cx| m.list.read(cx).selection.len() == 2),
    Input::Click(row0, Held::Ctrl),
];

const FORWARD_MANY: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    CTRL_ROW1,
    TWO_PICKED,
    Input::Click(row2, Held::Ctrl),
    until!(|m, cx| m.list.read(cx).selection.len() >= 3),
    until!(|_, _| screens::spotted("bulk-forward").is_some()),
    Input::Click(at!("bulk-forward"), Held::No),
    COMPOSE,
];

fn forwarded_many(_: &Scene, m: &Mailer, cx: &App) -> bool {
    model(m, cx).is_some_and(|c| c.subject.is_empty() && c.attachments().count() >= 3 && c.attachments().all(|(_, a)| a.name.ends_with(".eml")))
}

const FORWARD_ONE: &[Input] = &[READY, Input::Right(row1), MENU, Input::Keys(&["down", "down", "down", "enter"]), COMPOSE];

const SUMMARY: &[Input] = &[READY, LEFT, OPENED, CTRL_ROW1, TWO_PICKED, Input::Click(row2, Held::Ctrl)];

// --- вторая партия: прокрутка, закладки, вложения, срок напоминания, меню, стопка, настройки, панели, цвета ---

const SETTLE: Input = Input::Until(|_, _, _| pause(250));
const FORGET: Input = Input::Run(forget);
const CLOSE_TAB: Input = Input::Keys(&["ctrl-w"]);
const MAIN: Input = until!(|m, _| m.tabs.is_active(TabKind::Main));

fn forget(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    screens::forget_spots();
    Ok(())
}

fn settings_quick(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.open_settings(Some(SettingsPage::Quick), window, cx);
    Ok(())
}

fn settings_reading(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.open_settings(Some(SettingsPage::Reading), window, cx);
    Ok(())
}

fn settings_look(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.open_settings(Some(SettingsPage::Look), window, cx);
    Ok(())
}

/// «Настройки → Клавиатура», курсор в поле поиска команд.
fn settings_keys(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.open_settings(Some(SettingsPage::Keys), window, cx);
    let Some(view) = m.settings.view.clone() else { return Err("no settings view".into()) };
    let search = view.read(cx).keys.search.clone();
    search.update(cx, |s, cx| s.focus(window, cx));
    Ok(())
}

// Раздел 10, шаг 7.

/// Сдвиг списка вниз, px.
fn list_y(m: &Mailer, cx: &App) -> usize {
    f32::from(m.list.read(cx).scroll.offset().y).abs() as usize
}

/// Ползунок полосы прокрутки — вверху дорожки, у правого края списка.
fn thumb(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let b = m.list.read(cx).bounds.get();
    (b.size.width > px(0.)).then(|| point(b.origin.x + b.size.width - px(8.), b.origin.y + px(20.)))
}

fn thumb_down(m: &Mailer, w: &Window, cx: &App) -> Option<Point<Pixels>> {
    thumb(m, w, cx).map(|p| point(p.x, p.y + px(240.)))
}

const SCROLL: &[Input] = &[
    READY,
    Input::Move(thumb),
    Input::Drag(thumb, thumb_down),
    until!(|m, cx| list_y(m, cx) > 300 && keep(list_y(m, cx))),
    until!(|_, _| screens::spotted("mb-sent").is_some()),
    Input::Click(at!("mb-sent"), Held::No),
    until!(|m, cx| m.list.read(cx).mailbox.as_deref() == Some("sent") && rows_loaded(m.list.read(cx)) && list_y(m, cx) == 0),
    Input::Click(at!("mb-inbox"), Held::No),
];

fn scroll_kept(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).mailbox.as_deref() == Some("inbox") && list_y(m, cx) == kept() && rows_loaded(m.list.read(cx))
}

// Раздел 19, шаг 6.

/// Письмо без беседы и пересылка в беседе: таких строк в демо нет, сцена делает их из имеющихся.
fn solo(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    let rows = m.cache.page("inbox", 0, 8).map_err(|e| e.to_string())?;
    let Some(mut letter) = rows.into_iter().find(|r| !r.id.starts_with("chain")) else { return Err("no letter".into()) };
    letter.thread_id = "solo".into();
    let reply = m.cache.rows_by_ids(&[FORWARDED.into()]).map_err(|e| e.to_string())?.pop();
    let Some(mut forward) = reply else { return Err("no demo conversation in the cache".into()) };
    forward.subject = "FW: Пилот SIEM: сроки".into(); // i18n-ok: тема письма сцены
    letter.id.clone_into(&mut OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    m.cache.upsert_emails(&[letter, forward]).map_err(|e| e.to_string())?;
    m.list.update(cx, |l, cx| l.refresh(cx));
    Ok(())
}

/// Письмо беседы, ставшее пересылкой, и его номер в ленте (ответ с цитатой).
const FORWARDED: &str = "chain2";
const FORWARDED_N: &str = "feed-hidden-1";
const REPLY_N: &str = "feed-hidden-2";

/// Строка запомненного письма.
fn kept_row(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let open = OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let list = m.list.read(cx);
    let i = list.rows.1.iter().position(|r| r.id == *open)?;
    row(m, cx, i + list.rows.0)
}

const SOLO: &[Input] = &[
    READY,
    Input::Click(kept_row, Held::No),
    OPENED,
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| r.row.thread_id == "solo")),
    FORGET,
    SETTLE,
    until!(|m, cx| m.reading.read(cx).feed.is_none() && screens::spotted("thread-toggle").is_none() && screens::spotted("mb-inbox").is_some()),
    Input::Run(chain_feed),
    until!(|m, cx| feed_ready(m, cx)),
    FORGET,
    SETTLE,
];

fn forward_whole(_: &Scene, m: &Mailer, cx: &App) -> bool {
    feed_ready(m, cx)
        && screens::spotted("thread-toggle").is_some()
        && screens::spotted(FORWARDED_N).is_none()
        && screens::spotted(REPLY_N).is_some()
        && m.reading.read(cx).feed.as_ref().is_some_and(|f| f.conv.messages.iter().any(|x| postman_core::thread::is_forward(&x.row.subject)))
}

// Раздел 23, шаг 8.

const DRAFT: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Keys(&["ctrl-r"]),
    INLINE,
    Input::Text("ответ"),                                                                             // i18n-ok: ввод сценария
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.body.contains("ответ"))), // i18n-ok: ввод сценария
    until!(|m, _| m.compose.inline.as_ref().is_some_and(|i| keep(i.id as usize))),
    Input::Keys(&["escape"]),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| c.read(cx).model.confirm_close)),
    Input::Click(at!("close-save"), Held::No),
    until!(|m, cx| m.compose.inline_view().is_some_and(|c| {
        let c = &c.read(cx).model;
        !c.confirm_close && matches!(c.status, Status::NoServer(Purpose::Save)) && c.body.contains("ответ") // i18n-ok: ввод сценария
    }) && m.toasts.read(cx).latest().is_some_and(|t| t == tr!("needs-server"))),
    // Тост лежит поверх кнопки «Развернуть».
    Input::Click(at!("notice"), Held::No),
    until!(|m, cx| m.toasts.read(cx).on_screen() == 0),
    Input::Click(at!("expand"), Held::No),
    COMPOSE,
];

fn same_draft(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let tab = matches!(m.tabs.active().kind, TabKind::Compose(id) if id as usize == kept());
    tab && m.compose.inline.is_none() && letters(m) == 1 && body_has(m, cx, "ответ") // i18n-ok: ввод сценария
}

// Раздел 25, шаги 7–9.

/// Закладки включены и список пересчитан.
fn tabs_on(m: &Mailer, cx: &App, n: usize) -> bool {
    let list = m.list.read(cx);
    list.query.criteria.quick.len() == n && !list.computing && rows_loaded(list)
}

fn threat(r: &EmailRow) -> bool {
    r.has_keyword("$phishing")
}

fn due(r: &EmailRow) -> bool {
    postman_core::marks::has(&r.keywords, Kind::Due)
}

const TWO_TABS: &[Input] = &[
    READY,
    until!(|_, _| screens::spotted("quick-tab-Threats").is_some()),
    Input::Click(at!("quick-tab-Threats"), Held::No),
    until!(|m, cx| tabs_on(m, cx, 1) && m.list.read(cx).rows.1.iter().all(threat) && keep(m.list.read(cx).count)),
    Input::Click(at!("quick-tab-Due"), Held::No),
    until!(|m, cx| {
        let list = m.list.read(cx);
        tabs_on(m, cx, 2) && list.count > kept() && list.rows.1.iter().all(|r| threat(r) || due(r)) && list.rows.1.iter().any(due)
    }),
    until!(|m, cx| keep(m.list.read(cx).count)),
    Input::Click(at!("funnel"), Held::No),
    until!(|m, cx| m.filters.read(cx).panel),
    until!(|_, _| screens::spotted("flt-Unread").is_some()),
    Input::Click(at!("flt-Unread"), Held::No),
];

fn two_tabs(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    list.query.criteria.checks.contains(&Check::Unread)
        && !list.computing
        && list.count < kept()
        && list.rows.1.iter().all(|r| !r.seen && (threat(r) || due(r)))
}

fn quick_row(cx: &App, q: Listed) -> Option<usize> {
    crate::prefs::get(cx).quick.items.iter().position(|i| i.filter == q)
}

fn mine_toggle(_: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    screens::spotted(&format!("settings-quick-toggle-{}", quick_row(cx, Listed::Mine)?))
}

fn expiring_up(_: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    screens::spotted(&format!("settings-quick-up-{}", quick_row(cx, Listed::Expiring)?))
}

const HIDE_TABS: &[Input] = &[
    READY,
    until!(|_, _| screens::spotted("quick-hide").is_some()),
    Input::Click(at!("quick-tab-ToMe"), Held::No),
    until!(|m, cx| m.list.read(cx).query.criteria.quick.contains(&Tab::ToMe)),
    Input::Click(at!("quick-hide"), Held::No),
    until!(|m, cx| {
        crate::prefs::get(cx).quick.hidden
            && Mailer::quick_tabs(cx).is_empty()
            && m.toasts.read(cx).latest().is_some_and(|t| t == tr!("filters-quick-hidden"))
            && m.filters.read(cx).query.criteria.quick.is_empty()
    }),
    Input::Click(at!("bar-more"), Held::No),
    until!(|m, cx| m.bar.read(cx).more_open),
    until!(|_, _| screens::spotted("more-quick").is_some()),
    Input::Click(at!("more-quick"), Held::No),
    until!(|_, cx| !crate::prefs::get(cx).quick.hidden && Mailer::quick_tabs(cx).len() == Listed::ALL.len()),
    SETTLE,
    Input::Click(at!("quick-tab-ToMe"), Held::No),
    until!(|m, cx| m.list.read(cx).query.criteria.quick.contains(&Tab::ToMe)),
    Input::Run(settings_quick),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Quick, cx)),
    until!(|_, _| screens::spotted("settings-quick-toggle-3").is_some()),
    Input::Click(mine_toggle, Held::No),
    until!(|_, cx| !Mailer::quick_tabs(cx).contains(&Tab::ToMe)),
    until!(|m, cx| !m.filters.read(cx).query.criteria.quick.contains(&Tab::ToMe)),
    Input::Click(expiring_up, Held::No),
    until!(|_, cx| quick_row(cx, Listed::Expiring) == Some(3)),
    SETTLE,
    Input::Click(expiring_up, Held::No),
    until!(|_, cx| quick_row(cx, Listed::Expiring) == Some(2)),
    CLOSE_TAB,
    MAIN,
    FORGET,
    SETTLE,
];

fn tabs_moved(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let order = Mailer::quick_tabs(cx) == [Tab::Threats, Tab::Due, Tab::Expiring, Tab::Waiting];
    let on_screen = match (screens::spotted("quick-tab-Expiring"), screens::spotted("quick-tab-Waiting")) {
        (Some(a), Some(b)) => a.x < b.x,
        _ => false,
    };
    order && on_screen && screens::spotted("quick-tab-ToMe").is_none() && m.filters.read(cx).query.criteria.quick.is_empty()
}

/// Закладка «Угрозы» у нижнего края карточки списка.
fn tabs_at_list_bottom(m: &Mailer, cx: &App) -> bool {
    let b = m.list.read(cx).bounds.get();
    screens::spotted("quick-tab-Threats").is_some_and(|p| {
        let gap = f32::from(b.origin.y + b.size.height - p.y);
        (-12. ..48.).contains(&gap)
    })
}

/// Середина страницы настроек: колесо до строк ниже края окна.
fn settings_middle(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    Some(point(px(640.), px(400.)))
}

const SETTINGS_DOWN: Input = Input::Wheel(settings_middle, 560.);

const PANES: &[Input] = &[
    READY,
    Input::Run(settings_reading),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Reading, cx)),
    until!(|_, _| screens::spotted("settings-pane-1").is_some()),
    SETTINGS_DOWN,
    SETTLE,
    Input::Click(at!("settings-pane-1"), Held::No),
    until!(|_, cx| crate::prefs::get(cx).reading.pane == Pane::Bottom),
    CLOSE_TAB,
    MAIN,
    FORGET,
    SETTLE,
    until!(|m, cx| tabs_at_list_bottom(m, cx)),
    Input::Run(settings_reading),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Reading, cx)),
    SETTLE,
    SETTINGS_DOWN,
    SETTLE,
    Input::Click(at!("settings-pane-2"), Held::No),
    until!(|_, cx| crate::prefs::get(cx).reading.pane == Pane::Hidden),
    CLOSE_TAB,
    MAIN,
    FORGET,
    SETTLE,
    // Письма нет — список на всю ширину, закладки на месте.
    until!(|_, _| screens::spotted("quick-tab-Threats").is_some()),
    until!(|m, cx| m.list.read(cx).bounds.get().size.width > px(f32::from(Layout::LIST) + 100.)),
    LEFT,
    OPENED,
    FORGET,
    SETTLE,
];

fn tabs_gone(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.reading_instead_of_list(cx) && screens::spotted("quick-tab-Threats").is_none() && screens::spotted("mb-inbox").is_some()
}

// Раздел 27, шаги 5, 8.

/// Карточка вложения в ленте открытого письма.
fn card(m: &Mailer, cx: &App, blob: &str) -> Option<Point<Pixels>> {
    let items = crate::attachments::items(screens::reading(m, cx)?);
    let i = items.iter().position(|a| a.row.blob_id == blob)?;
    screens::spotted(&format!("att-card-{i}"))
}

fn zip_card(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    card(m, cx, "zip")
}

const ZIP: &[Input] =
    &[READY, Input::Click(outlook_row, Held::No), OPENED, until!(|m, cx| card(m, cx, "zip").is_some()), Input::Click(zip_card, Held::No)];

/// Файл с именем `name` есть в каталоге или глубже.
fn has_file(dir: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| e.file_name() == name || (e.path().is_dir() && has_file(&e.path(), name)))
}

/// Каталог данных прогона (`tests/manual.rs`: временный `LOCALAPPDATA`).
fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from)
}

fn zip_listed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let shown = screens::reading(m, cx).and_then(|r| r.preview.as_ref());
    let Some(PvState::Ready(Shown::Archive(list))) = shown.map(|p| &p.state) else { return false };
    let names = || list.entries.iter().map(|e| e.name.as_str());
    let listed = names().any(|n| n == "tools/collect.bat") && names().any(|n| n == "report.docm");
    let on_disk = [data_dir(), std::env::temp_dir().join("Postman")].iter().any(|d| has_file(d, "collect.bat") || has_file(d, "report.docm"));
    listed && postman_core::attach::dangerous(names()) == 2 && !on_disk
}

/// Куда сцена сохраняет: мимо диалога Windows, в каталог данных прогона.
fn saved_dir() -> PathBuf {
    data_dir().join("save-all")
}

fn saved_files() -> Vec<PathBuf> {
    std::fs::read_dir(saved_dir()).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

fn save_all(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    std::fs::create_dir_all(saved_dir()).map_err(|e| e.to_string())?;
    m.reading.update(cx, |r, cx| r.save_all_in(saved_dir(), cx));
    Ok(())
}

/// Вложений для просмотра в письме Outlook демо.
const ATTACHED: usize = 5;

const SAVE_ALL: &[Input] = &[
    READY,
    Input::Click(outlook_row, Held::No),
    OPENED,
    Input::Run(save_all),
    until!(|m, cx| saved_files().len() == ATTACHED
        && m.toasts.read(cx).latest().is_some_and(|t| t == tr!("att-saved-all", n = ATTACHED, dir = saved_dir().display().to_string()))),
    Input::Run(save_all),
];

fn saved_twice(_: &Scene, _: &Mailer, _: &App) -> bool {
    let files = saved_files();
    let copies = files.iter().filter(|f| f.file_stem().is_some_and(|s| s.to_string_lossy().ends_with(" (2)"))).count();
    let marked = files.iter().all(|f| {
        let mut ads = f.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        std::fs::read_to_string(ads).is_ok_and(|z| z.contains("ZoneId=3"))
    });
    files.len() == ATTACHED * 2 && copies == ATTACHED && marked
}

// Раздел 28, шаг 3.

fn planner(_: &Scene, m: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    if let Some(d) = &mut m.screens {
        d.planner = true;
    }
    Ok(())
}

fn second() -> i8 {
    Zoned::now().second()
}

/// Следующая минута, как в поле: срок ближе нельзя, точность меток — минута.
fn next_minute() -> String {
    let at = Zoned::now().checked_add(jiff::Span::new().minutes(1)).unwrap_or_else(|_| Zoned::now());
    at.strftime("%d.%m.%Y %H:%M").to_string()
}

const ARRIVE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).count) && keep_open(m, cx)),
    // До следующей минуты — 14–24 с: срок не успеет пройти за набором, а ожидание срока короче предела шага.
    // Сами шаги ожидания — тоже короче предела, поэтому их три.
    Input::Until(|_, _, _| !(21..45).contains(&second())),
    Input::Until(|_, _, _| (8..21).contains(&second())),
    Input::Until(|_, _, _| (36..46).contains(&second())),
    // Из палитры: срок набирается в строке палитры, как в разделе 28.
    Input::Keys(&["ctrl-k"]),
    Input::Text("Напомнить"), // i18n-ok: ввод сценария
    PALETTE_SHOWN,
    ENTER,
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.kind == Kind::Remind)),
    until!(|_, _| screens::spotted("lifecycle-pick-date").is_some()),
    Input::Click(at!("lifecycle-pick-date"), Held::No),
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.input.is_some())),
    Input::Typed(next_minute),
    ENTER,
    until!(|m, cx| m.lifecycle.picker.is_none() && gone(m, cx)),
    until!(|m, _| !m.lifecycle.cards.is_empty()),
    until!(|m, cx| reminded_on_top(m, cx)),
    // Письмо открыто и в срок: метка в шапке — по новой строке.
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| kept_is(&r.row.id) && postman_core::marks::find(&r.row.keywords, Kind::Remind).is_none())),
    until!(|_, _| screens::spotted("reminder-open-0").is_some()),
    Input::Click(at!("reminder-open-0"), Held::No),
    until!(|m, cx| m.lifecycle.cards.is_empty() && m.list.read(cx).rows.1.iter().any(|r| kept_is(&r.id) && r.seen)),
    until!(|m, cx| screens::reading(m, cx).is_some_and(|r| kept_is(&r.row.id))),
];

/// Запомненное письмо (`keep_open`).
fn kept_is(id: &str) -> bool {
    *OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) == id
}

fn reminded_on_top(m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    list.row(0).is_some_and(|r| kept_is(&r.id) && postman_core::lifecycle::reminded(r))
        && list.layout.group_of(0) == Some(&Group::Top)
        && list.count == kept()
}

fn reminder_done(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).layout.group_of(0) != Some(&Group::Top)
}

// Раздел 29, шаг 7.

/// Пункты меню письма (порядок — `menu.rs`).
const ITEM_FIRST_MANY: usize = 3;
const ITEM_CATEGORY: usize = 6;
const ITEM_PHISHING: usize = 16;

const MENU_MOUSE: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).selected.unwrap_or(0))),
    Input::Right(row2),
    MENU,
    until!(|m, cx| {
        let list = m.list.read(cx);
        list.menu_row == Some(2) && list.selection.is_empty() && list.selected == Some(kept()) && m.menu.as_ref().is_some_and(|x| x.sub().is_none())
    }),
    until!(|_, _| screens::spotted("menu-item-11").is_some()),
    Input::Move(at!("menu-item-6")),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Category))),
    Input::Move(at!("menu-item-11")),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Move))),
    Input::Move(at!("menu-item-15")),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::View))),
    Input::Move(at!("menu-item-0")),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub().is_none())),
    Input::Move(at!("menu-item-6")),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Category))),
    until!(|_, _| screens::spotted("menu-cat-0").is_some()),
    Input::Click(at!("menu-cat-0"), Held::No),
    until!(|m, cx| m.menu.is_none() && toast_starts(m, cx, "Категория")), // i18n-ok: ввод сценария
    Input::Right(row3),
    MENU,
    // Меню открыто от середины строки вниз: строка выше него — мимо.
    LEFT,
];

fn menu_done(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.menu.is_none() && m.list.read(cx).menu_row.is_none()
}

const MENU_KEYS: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    until!(|m, cx| keep(m.list.read(cx).selected.unwrap_or(0))),
    Input::Keys(&["shift-f10"]),
    MENU,
    until!(|m, cx| m.list.read(cx).menu_row == Some(kept())),
    Input::Keys(&["down", "down"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.cursor().0 == 2)),
    Input::Keys(&["escape"]),
    until!(|m, cx| m.menu.is_none() && m.list.read(cx).menu_row.is_none()),
    Input::Keys(&["menu"]),
    MENU,
    Input::Keys(&["down", "down", "down", "down", "down", "down"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.cursor().0 == ITEM_CATEGORY)),
    Input::Keys(&["right"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Category))),
    Input::Keys(&["down"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.cursor().1 == 1)),
    Input::Keys(&["left"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub().is_none())),
    Input::Keys(&["right"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Category))),
    Input::Keys(&["j", "k", "7"]),
    SETTLE,
    until!(|m, cx| m.menu.is_some() && m.list.read(cx).selected == Some(kept()) && m.toasts.read(cx).on_screen() == 0),
    // Меню берёт одно письмо строки, а у него уже стоят категории первых двух пунктов: пункт снял бы её. Третий — ставит.
    Input::Keys(&["down", "down"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.cursor().1 == 2)),
    ENTER,
];

const MENU_MANY: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    CTRL_ROW1,
    TWO_PICKED,
    Input::Right(row1),
    MENU,
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.letters() >= 2 && x.cursor().0 == ITEM_FIRST_MANY && x.on_enabled())),
    Input::Keys(&["up"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.cursor().0 == ITEM_PHISHING)),
    Input::Keys(&["escape"]),
    until!(|m, cx| m.menu.is_none() && m.list.read(cx).selection.len() >= 2),
    Input::Right(row3),
    MENU,
    until!(|m, cx| m.list.read(cx).menu_row == Some(3) && m.menu.as_ref().is_some_and(|x| x.cursor().0 == 0)),
    Input::Keys(&["escape"]),
];

// Раздел 30, шаг 7.

/// Чип «Уточнить» такого вида.
fn chip(m: &Mailer, cx: &App, wanted: fn(&Refine) -> bool) -> Option<Point<Pixels>> {
    let list = m.list.read(cx);
    let i = list.found.as_ref()?.chips.iter().position(|(r, _)| wanted(r))?;
    screens::spotted(&format!("refine-{i}"))
}

fn attachment_chip(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    chip(m, cx, |r| matches!(r, Refine::Attachment))
}

fn unread_chip(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    chip(m, cx, |r| matches!(r, Refine::Unread))
}

fn query_has(m: &Mailer, cx: &App, part: &str) -> bool {
    m.search.read(cx).value().contains(part)
}

/// Результаты после чипа разобраны.
fn refined(m: &Mailer, cx: &App, part: &str, on: bool) -> bool {
    let list = m.list.read(cx);
    let done = query_has(m, cx, part) == on && m.find.active() && !list.computing;
    done && list.found.as_ref().is_some_and(|f| f.chips.iter().any(|(r, set)| *set == on && r.apply("").contains(part)))
}

/// Правая граница строки чипов: крайний чип за ней, пока строку не прокрутили.
fn last_chip_x(m: &Mailer, cx: &App) -> Option<usize> {
    let n = m.list.read(cx).found.as_ref()?.chips.len();
    Some(f32::from(screens::spotted(&format!("refine-{}", n.checked_sub(1)?))?.x) as usize)
}

const REFINE: &[Input] = &[
    READY,
    Input::Keys(&["ctrl-e"]),
    Input::Text("квартал"), // i18n-ok: ввод сценария
    until!(|m, cx| found(m, cx) && m.list.read(cx).found.as_ref().is_some_and(|f| !f.chips.is_empty())),
    until!(|m, cx| chip(m, cx, |r| matches!(r, Refine::Attachment)).is_some() && keep(m.list.read(cx).count)),
    Input::Click(attachment_chip, Held::No),
    until!(|m, cx| refined(m, cx, "has:attachment", true) && m.list.read(cx).count < kept()),
    Input::Click(attachment_chip, Held::No),
    until!(|m, cx| refined(m, cx, "has:attachment", false) && m.list.read(cx).count == kept()),
    until!(|m, cx| chip(m, cx, |r| matches!(r, Refine::Unread)).is_some()),
    Input::Click(unread_chip, Held::No),
    until!(|m, cx| refined(m, cx, "is:unread", true) && m.list.read(cx).count < kept()),
    Input::Click(unread_chip, Held::No),
    until!(|m, cx| refined(m, cx, "is:unread", false) && m.list.read(cx).count == kept()),
    SETTLE,
    until!(|m, cx| last_chip_x(m, cx).is_some_and(keep)),
    Input::Wheel(at!("refine-chips"), 300.),
];

fn chips_scrolled(_: &Scene, m: &Mailer, cx: &App) -> bool {
    last_chip_x(m, cx).is_some_and(|x| x < kept())
}

// Раздел 31, шаги 3, 8.

fn stack(s: &Scene, m: &mut Mailer, w: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    crate::prefs::update(cx, |p| p.reading.set_folder("inbox", Some(Conversation::Stack)));
    chain(s, m, w, cx)
}

fn unfolded(m: &Mailer, cx: &App, id: &str) -> bool {
    m.reading.read(cx).feed.as_ref().is_some_and(|f| f.open.contains(id))
}

/// Раскрытых писем стопки.
fn unfolded_n(m: &Mailer, cx: &App) -> usize {
    m.reading.read(cx).feed.as_ref().map_or(0, |f| f.open.len())
}

fn opened(m: &Mailer, cx: &App, id: &str) -> bool {
    screens::reading(m, cx).is_some_and(|r| r.row.id == id)
}

const STACK: &[Input] = &[
    until!(|m, cx| m.reading.read(cx).stacked(cx) && feed_loaded(m, cx) && !unfolded(m, cx, "chain1")),
    until!(|_, _| screens::spotted("stack-row-0").is_some()),
    Input::Click(at!("stack-row-0"), Held::No),
    until!(|m, cx| unfolded(m, cx, "chain1") && opened(m, cx, "chain1")),
    until!(|_, _| screens::spotted("stack-head-0").is_some()),
    Input::Click(at!("stack-head-0"), Held::No),
    until!(|m, cx| !unfolded(m, cx, "chain1") && opened(m, cx, "chain1")),
    ENTER,
    until!(|m, cx| unfolded(m, cx, "chain1")),
    Input::Click(at!("stack-all"), Held::No),
    until!(|m, cx| unfolded_n(m, cx) == postman_demo::CHAIN.len()),
    SETTLE,
    Input::Click(at!("stack-all"), Held::No),
    until!(|m, cx| unfolded_n(m, cx) == 0),
    Input::Keys(&["ctrl-right"]),
    until!(|m, cx| opened(m, cx, "chain2") && unfolded(m, cx, "chain2")),
    Input::Keys(&["ctrl-left"]),
    until!(|m, cx| opened(m, cx, "chain1") && unfolded(m, cx, "chain1")),
    SETTLE,
    Input::Click(at!("stack-all"), Held::No),
    until!(|m, cx| unfolded_n(m, cx) == postman_demo::CHAIN.len()),
    SETTLE,
    until!(|_, _| screens::spotted("thread-reply-2").is_some()),
    Input::Click(at!("thread-reply-2"), Held::No),
];

fn feed_loaded(m: &Mailer, cx: &App) -> bool {
    m.reading.read(cx).feed.as_ref().is_some_and(|f| f.conv.len() == postman_demo::CHAIN.len() && f.ready())
}

fn stack_replied(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.compose.inline.as_ref().is_some_and(|i| i.letter == "chain3") && m.compose.inline_view().is_some_and(|c| c.read(cx).drawn())
}

/// Вид «как Outlook» у «Входящих» в строке настроек: сегмент `fi * 4 + 2`.
fn inbox_outlook(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let view = m.settings.view.as_ref()?.read(cx);
    let fi = view.info.folders.iter().position(|(id, _)| id == "inbox")?;
    screens::spotted(&format!("settings-folder-view-{}", fi * 4 + 2))
}

const VIEW_SETTINGS: &[Input] = &[
    READY,
    LEFT,
    OPENED,
    Input::Run(settings_reading),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Reading, cx)),
    until!(|_, _| screens::spotted("settings-conv-2").is_some()),
    Input::Click(at!("settings-conv-2"), Held::No),
    until!(|_, cx| crate::prefs::get(cx).reading.conversation == Conversation::Outlook),
    Input::Keys(&["ctrl-tab"]),
    MAIN,
    until!(|m, cx| !m.list.read(cx).flat_mode(cx) && m.list.read(cx).outline.is_some()),
    Input::Keys(&["ctrl-tab"]),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Reading, cx)),
    SETTLE,
    Input::Click(at!("settings-conv-0"), Held::No),
    until!(|_, cx| crate::prefs::get(cx).reading.conversation == Conversation::Feed),
    SETTLE,
    Input::Click(inbox_outlook, Held::No),
    until!(|m, cx| view_of(m, cx) == Some(Conversation::Outlook)),
    Input::Keys(&["ctrl-tab"]),
    MAIN,
    until!(|m, cx| !m.list.read(cx).flat_mode(cx) && m.list.read(cx).outline.is_some() && !m.list.read(cx).computing),
    SETTLE,
    RIGHT,
    MENU,
    Input::Keys(TO_VIEW),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::View))),
    Input::Keys(&["down", "down", "down", "enter"]),
];

// Раздел 32, шаг 5.

const NEXT_MATCH: &str = "search.next_match";

/// Клавиши команды в столбце Outlook.
fn keys_of(id: &str, cx: &App) -> Vec<String> {
    let set = crate::keymap::KeySet::of(&crate::prefs::get(cx));
    let Some(c) = pets_commands::find(crate::commands::all(), id) else { return Vec::new() };
    crate::keymap::column_keys(&crate::commands::SINGLE, &set, c, crate::keymap::Column::Outlook)
}

const HIT_KEYS: &[Input] = &[
    READY,
    until!(|_, cx| keys_of(NEXT_MATCH, cx) == ["f3"] && keys_of("search.prev_match", cx) == ["shift-f3"]),
    Input::Run(settings_keys),
    Input::Text("следующее совпадение"), // i18n-ok: ввод сценария
    until!(|m, cx| m.settings.view.as_ref().is_some_and(|v| v.read(cx).keys.query == "следующее совпадение")), // i18n-ok: ввод сценария
    SETTLE,
    until!(|_, _| screens::spotted("settings-keys-edit-0").is_some()),
    Input::Click(at!("settings-keys-edit-0"), Held::No),
    until!(|m, cx| m.settings.view.as_ref().is_some_and(|v| v.read(cx).keys.recording == Some(NEXT_MATCH))),
    Input::Keys(&["f4"]),
    until!(|_, cx| keys_of(NEXT_MATCH, cx) == ["f4"]),
    CLOSE_TAB,
    MAIN,
    Input::Keys(&["ctrl-e"]),
    Input::Text("квартал"), // i18n-ok: ввод сценария
    until!(|m, cx| found(m, cx)),
    Input::Click(row1, Held::No),
    until!(|m, cx| m.reading.read(cx).hits.get() > 1 && m.reading.read(cx).hit == 0),
    Input::Keys(&["f3"]),
    SETTLE,
    until!(|m, cx| m.reading.read(cx).hit == 0),
    Input::Keys(&["f4"]),
];

// Раздел 37, шаг 2.

fn to_all(m: &Mailer, cx: &App) -> bool {
    m.compose.last().is_some_and(|c| {
        let c = c.read(cx);
        c.drawn() && c.model.subject.to_lowercase().starts_with("re:") && format!("{};{}", c.model.to, c.model.cc).contains("team@corp.example")
    })
}

const AFE: &[Input] = &[
    READY,
    Input::Click(outlook_row, Held::No),
    OPENED,
    until!(|m, cx| keep_open(m, cx)),
    Input::Keys(&["a"]),
    until!(|m, cx| to_all(m, cx)),
    Input::Keys(&["escape"]),
    until!(|m, _| m.compose.last().is_none()),
    Input::Keys(&["f"]),
    until!(|m, cx| replying(m, cx, "FW:")),
    Input::Keys(&["escape"]),
    until!(|m, _| m.compose.last().is_none()),
    Input::Keys(&["e"]),
];

// T1-04г.

/// Папка отступает от родителя на 16 px: центры шевронов.
fn indented(child: &str, parent: &str) -> bool {
    match (screens::spotted(&format!("mb-fold-{child}")), screens::spotted(&format!("mb-fold-{parent}"))) {
        (Some(a), Some(b)) => f32::from(a.x - b.x) == 16.,
        _ => false,
    }
}

fn inbox_unread(m: &Mailer, cx: &App) -> usize {
    m.folders.read(cx).mailboxes.iter().find(|b| b.id == "inbox").map_or(0, |b| b.unread as usize)
}

const DEEP: &str = "inbox-clients-north-contracts";

const TREE: &[Input] = &[
    READY,
    until!(|_, _| indented("inbox-clients", "inbox") && indented("inbox-clients-north", "inbox-clients") && indented(DEEP, "inbox-clients-north")),
    until!(|m, cx| keep(inbox_unread(m, cx))),
    Input::Click(at!("mb-fold-inbox"), Held::No),
    until!(|m, cx| m.folders.read(cx).is_folded("inbox")),
    FORGET,
    SETTLE,
    until!(|_, _| screens::spotted("mb-fold-inbox-clients").is_none() && screens::spotted("mb-fold-inbox").is_some()),
    Input::Keys(&["ctrl-k"]),
    Input::Text("Договоры"), // i18n-ok: ввод сценария
    PALETTE_SHOWN,
    ENTER,
    until!(|m, cx| m.palette.is_none() && m.folders.read(cx).current.as_deref() == Some(DEEP)),
    FORGET,
    SETTLE,
];

const REDO_TEXT: &str = "abc";

const GO_TO: &[Input] = &[
    READY,
    Input::Click(at!("mb-fold-inbox"), Held::No),
    until!(|m, cx| m.folders.read(cx).is_folded("inbox")),
    FORGET,
    SETTLE,
    Input::Keys(&["ctrl-y"]),
    until!(|m, cx| m.palette.as_ref().is_some_and(|p| p.view.read(cx).scope() == Scope::Folders && p.view.read(cx).shown() > 0)),
    Input::Text("Договоры"), // i18n-ok: ввод сценария
    PALETTE_SHOWN,
    ENTER,
    until!(|m, cx| m.palette.is_none() && m.folders.read(cx).current.as_deref() == Some(DEEP) && !m.folders.read(cx).is_folded("inbox")),
    Input::Keys(&["ctrl-e"]),
    Input::Text(REDO_TEXT),
    until!(|m, cx| m.search.read(cx).value() == REDO_TEXT),
    Input::Keys(&["ctrl-z"]),
    until!(|m, cx| m.search.read(cx).value() != REDO_TEXT),
    Input::Keys(&["ctrl-y"]),
];

fn went_and_redid(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.is_none() && m.search.read(cx).value() == REDO_TEXT && m.folders.read(cx).current.as_deref() == Some(DEEP)
}

// Решение владельца п. 10.

/// Письма беседы под меню: запоминает их шаг `THREAD_HEAD`.
static MEMBERS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Первая видимая строка беседы из нескольких писем.
fn thread_head(m: &Mailer, cx: &App) -> Option<usize> {
    let list = m.list.read(cx);
    let (offset, rows) = &list.rows;
    (*offset..offset + rows.len()).find(|i| matches!(list.thread_mark(*i), Mark::Head { .. }) && row(m, cx, *i).is_some())
}

fn head_place(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, thread_head(m, cx)?)
}

const THREAD_HEAD: Input = until!(|m, cx| {
    let list = m.list.read(cx);
    let members = thread_head(m, cx).map(|i| list.members_of(i)).unwrap_or_default();
    let many = members.len() > 1 && list.outline.is_some() && !list.computing;
    *MEMBERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = members;
    many
});
/// Без выделения: шапки «Выбрано N писем» нет, меню — над одним письмом.
const ONE_LETTER: Input = until!(|m, cx| m.list.read(cx).selection.is_empty() && m.menu.as_ref().is_some_and(|x| x.letters() == 1));

const THREAD_REMIND: &[Input] = &[
    READY,
    THREAD_HEAD,
    Input::Right(head_place),
    MENU,
    ONE_LETTER,
    Input::Keys(TO_REMIND),
    ENTER,
    until!(|m, _| m.lifecycle.picker.as_ref().is_some_and(|p| p.kind == Kind::Remind && p.rest.is_empty())),
    until!(|_, _| screens::spotted("lifecycle-pick-0").is_some()),
    Input::Click(at!("lifecycle-pick-0"), Held::No),
];

fn one_reminded(_: &Scene, m: &Mailer, _: &App) -> bool {
    let ids = MEMBERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let rows = m.cache.rows_by_ids(&ids).unwrap_or_default();
    rows.len() == ids.len() && rows.iter().filter(|r| postman_core::marks::find(&r.keywords, Kind::Remind).is_some()).count() == 1
}

const THREAD_FORWARD: &[Input] =
    &[READY, THREAD_HEAD, Input::Right(head_place), MENU, ONE_LETTER, Input::Keys(&["down", "down", "down", "enter"]), COMPOSE];

fn tree_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let f = m.folders.read(cx);
    !f.is_folded("inbox") && indented(DEEP, "inbox-clients-north") && inbox_unread(m, cx) == kept()
}

// T1-05б.

/// Высота, на которой тянем разделители.
const SPLIT_Y: f32 = 400.;

fn split_folders(m: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    Some(point(px(m.panes.0 + 4.), px(SPLIT_Y)))
}

fn split_list(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let b = m.list.read(cx).bounds.get();
    (b.size.width > px(0.)).then(|| point(b.origin.x + b.size.width + px(4.), px(SPLIT_Y)))
}

/// Левее любого минимума.
fn far_left(_: &Mailer, _: &Window, _: &App) -> Option<Point<Pixels>> {
    Some(point(px(8.), px(SPLIT_Y)))
}

fn split_list_left(m: &Mailer, w: &Window, cx: &App) -> Option<Point<Pixels>> {
    split_list(m, w, cx).map(|p| point(p.x - px(60.), p.y))
}

const SPLITS: &[Input] = &[
    READY,
    until!(|m, cx| m.list.read(cx).bounds.get().size.width > px(0.)),
    Input::Drag(split_folders, far_left),
    until!(|m, _| m.panes.0 == f32::from(Layout::MIN_FOLDERS)),
    SETTLE,
    Input::Drag(split_list, far_left),
    until!(|m, _| m.panes.1 == f32::from(Layout::MIN_LIST)),
    SETTLE,
    Input::Double(split_list),
    until!(|m, cx| m.panes.1 == W_LIST && crate::prefs::get(cx).layout.list.is_none()),
    SETTLE,
    Input::Double(split_folders),
];

const NARROW: &[Input] = &[
    READY,
    // Место есть за счёт чтения: папки и список — как были.
    Input::Resize(1160., 800.),
    SETTLE,
    until!(|m, _| m.panes == (W_FOLDERS, W_LIST)),
    Input::Resize(1000., 800.),
    until!(|m, _| m.panes.0 == W_FOLDERS && m.panes.1 < W_LIST && m.panes.1 > f32::from(Layout::MIN_LIST)),
    Input::Resize(760., 800.),
];

// T1-D.

const FOCUS: &[Input] = &[READY, LEFT, OPENED, Input::Move(row2), SETTLE];

const BLUR: &[Input] = &[READY, LEFT, OPENED, until!(|m, cx| m.list.read(cx).focused), Input::Keys(&["ctrl-e"]), SETTLE];

fn no_focus(_: &Scene, m: &Mailer, cx: &App) -> bool {
    !m.list.read(cx).focused && !m.folders.read(cx).focused && !m.contacts.read(cx).focused && m.list.read(cx).selected.is_some()
}

static TONES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn tones() -> Vec<String> {
    TONES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Тон выделения теперь новый: запомнить его.
fn new_tone(cx: &App) -> bool {
    let tone = pets_ui::theme::Color::from_hsla(crate::roles::look(cx).row_fill(true)).hex();
    let mut seen = TONES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fresh = !seen.contains(&tone);
    if fresh {
        seen.push(tone);
    }
    fresh
}

const ACCENTS: &[Input] = &[
    READY,
    until!(|_, cx| new_tone(cx)),
    Input::Run(settings_look),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Look, cx)),
    until!(|_, _| screens::spotted("settings-accent-4").is_some()),
    Input::Click(at!("settings-accent-1"), Held::No),
    until!(|_, cx| new_tone(cx)),
    Input::Click(at!("settings-accent-2"), Held::No),
    until!(|_, cx| new_tone(cx)),
    Input::Click(at!("settings-accent-3"), Held::No),
    until!(|_, cx| new_tone(cx)),
    Input::Click(at!("settings-accent-4"), Held::No),
    until!(|_, cx| new_tone(cx)),
    CLOSE_TAB,
    MAIN,
    LEFT,
    OPENED,
];

/// «Серая» в переключателе тем.
fn gray_seg(_: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let i = Themes::try_get(cx)?.state().ids().position(|id| id == crate::roles::GRAY)?;
    screens::spotted(&format!("settings-theme-{i}"))
}

const GRAY: &[Input] = &[
    READY,
    Input::Run(settings_look),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Look, cx)),
    SETTLE,
    Input::Click(gray_seg, Held::No),
    until!(|_, cx| crate::roles::look(cx).gray()),
    CLOSE_TAB,
    MAIN,
    LEFT,
    OPENED,
];

// Перезапуск.

const RESTART_SET: &[Input] = &[
    READY,
    until!(|m, cx| m.list.read(cx).bounds.get().size.width > px(0.)),
    Input::Drag(split_list, split_list_left),
    until!(|_, cx| crate::prefs::get(cx).layout.list.is_some()),
    until!(|_, _| screens::spotted("mb-fold-inbox").is_some()),
    Input::Click(at!("mb-fold-inbox"), Held::No),
    until!(|m, cx| m.folders.read(cx).is_folded("inbox")),
    Input::Run(settings_reading),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Reading, cx)),
    until!(|_, _| screens::spotted("settings-conv-1").is_some()),
    Input::Click(at!("settings-conv-1"), Held::No),
    until!(|_, cx| crate::prefs::get(cx).reading.conversation == Conversation::Stack),
    Input::Run(settings_look),
    until!(|m, cx| screens::settings_on(m, SettingsPage::Look, cx)),
    SETTLE,
    Input::Click(gray_seg, Held::No),
    until!(|_, cx| crate::roles::look(cx).gray()),
    // Настройки пишутся в фоне: успеть до выхода.
    Input::Until(|_, _, _| pause(800)),
];

const RESTART_CHECK: &[Input] = &[READY, until!(|_, _| screens::spotted("mb-fold-inbox-clients").is_some())];

fn restarted(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let p = crate::prefs::get(cx);
    let width = p.layout.list.is_some_and(|w| f32::from(w) == m.panes.1 && m.panes.1 < W_LIST);
    width && p.reading.conversation == Conversation::Stack && crate::roles::look(cx).gray() && !m.folders.read(cx).is_folded("inbox")
}

// S5-19, шаг 1.

fn invitation(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    m.set_section(Section::Calendar, cx);
    m.cal.update(cx, |c, cx| {
        c.go_to_demo_week(cx);
        c.select_pending(cx);
    });
    Ok(())
}

const INVITE: &[Input] = &[
    until!(|_, _| screens::spotted("accept").is_some() && screens::spotted("tentative").is_some() && screens::spotted("decline").is_some()),
    Input::Move(at!("tentative")),
    SETTLE,
];

/// Вернуть настройки своих сцен перед следующей.
fn reset(s: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
    if !s.name.starts_with("mail-") {
        return;
    }
    crate::prefs::update(cx, |p| {
        p.key_scheme = KeyScheme::Outlook;
        p.keys_single.clear();
        p.keytips = true;
        p.reading.thread_order = ThreadOrder::OldestFirst;
        p.reading.set_folder("inbox", None);
    });
}

fn start(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    Ok(())
}

fn mail_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    rows_loaded(m.list.read(cx))
}

fn menu_open(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.menu.is_some()
}

/// Письмо активной вкладки нарисовано: поля ввода есть.
fn drawn(_: &Scene, m: &Mailer, cx: &App) -> bool {
    screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn())
}

/// Модель письма активной вкладки.
fn model<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a Model> {
    screens::compose_tab(m).map(|c| &c.read(cx).model)
}

fn body_has(m: &Mailer, cx: &App, part: &str) -> bool {
    model(m, cx).is_some_and(|c| c.body.contains(part))
}

fn letters(m: &Mailer) -> usize {
    m.tabs.iter().filter(|t| matches!(t.kind, TabKind::Compose(_))).count()
}

fn only_main(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.iter().count() == 1 && m.tabs.is_active(TabKind::Main)
}

/// Последний тост начинается так (сцены — на `--lang ru`).
fn toast_starts(m: &Mailer, cx: &App, text: &str) -> bool {
    m.toasts.read(cx).latest().is_some_and(|t| t.starts_with(text))
}

/// Метки сроков у открытого письма — по строке списка: она меняется от действий.
fn badges(m: &Mailer, cx: &App) -> Vec<postman_core::lifecycle::Badge> {
    let Some(id) = screens::reading(m, cx).map(|r| r.row.id.clone()) else { return Vec::new() };
    m.list.read(cx).rows.1.iter().find(|r| r.id == id).map(|r| crate::lifecycle::row_badges(r, &Zoned::now())).unwrap_or_default()
}

/// Центр строки `i` списка (без прокрутки): высоты из раскладки, заголовки групп учтены.
fn row(m: &Mailer, cx: &App, i: usize) -> Option<Point<Pixels>> {
    let list = m.list.read(cx);
    let b = list.bounds.get();
    if b.size.width <= px(0.) {
        return None;
    }
    let item = list.layout.item_of(i)?;
    let top: Pixels = list.sizes.get(..item)?.iter().map(|s| s.height).sum();
    let h = list.sizes.get(item)?.height;
    Some(point(b.origin.x + b.size.width / 2., b.origin.y + top + h / 2.))
}

fn row0(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, 0)
}

fn row1(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, 1)
}

fn row2(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, 2)
}

fn row3(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, 3)
}

fn outlook_row(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, demo_row(m.list.read(cx), |b: &BodyRow| outlook_letter(b))?)
}

static KEPT: AtomicUsize = AtomicUsize::new(0);
static SINCE: Mutex<Option<Instant>> = Mutex::new(None);
static OPEN: Mutex<String> = Mutex::new(String::new());

/// Запомнить значение на шаг вперёд (условия без состояния); всегда `true`.
fn keep(v: usize) -> bool {
    KEPT.store(v, Ordering::Relaxed);
    true
}

fn kept() -> usize {
    KEPT.load(Ordering::Relaxed)
}

/// Запомнить открытое письмо; всегда `true`, если оно есть.
fn keep_open(m: &Mailer, cx: &App) -> bool {
    let Some(r) = screens::reading(m, cx) else { return false };
    r.row.id.clone_into(&mut OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    true
}

/// Запомненное письмо ушло из прочитанных строк (`count` свода бесед обновляется позже).
fn gone(m: &Mailer, cx: &App) -> bool {
    let open = OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    !m.list.read(cx).rows.1.iter().any(|r| r.id == *open)
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
