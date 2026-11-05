//! Сценарии с вводом области «почта и письмо» (`docs/MANUAL-TESTS.md`, карточки T1 и S5-19): имя сцены — `mail-<раздел>-<что>`.
//! Механика — `screens.rs`, запуск — тест `mail_drills` в `app-gpui/tests/manual.rs`. `ready` и `Until` проверяют обещанное шагом
//! по модели; цвет и вид — по снимку.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::*;
use jiff::Zoned;
use pets_i18n::tr;
use postman_cache::BodyRow;
use postman_core::lifecycle::Bad;
use postman_core::marks::Kind;
use postman_core::prefs::{Conversation, KeyScheme, ThreadOrder};
use postman_core::send_later::Reject;

use crate::Mailer;
use crate::compose::{Model, Purpose, Status, segments};
use crate::menu::Sub;
use crate::preview::PvState;
use crate::screens::{self, Held, Input, Scene, demo_row, outlook_letter, rows_loaded};
use crate::tabs::TabKind;

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
    scene("mail-28-remind", count_fell, REMIND),
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
    scene("mail-29-move", count_fell, MOVE),
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
    until!(|m, cx| keep(m.list.read(cx).count)),
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
    until!(|m, cx| keep(m.list.read(cx).count)),
    Input::Keys(&["ctrl-shift-v"]),
    until!(|m, _| m.menu.as_ref().is_some_and(|x| x.sub() == Some(Sub::Move))),
    ENTER,
];

fn count_fell(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).count < kept()
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
