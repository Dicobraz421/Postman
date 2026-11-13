//! `--screens <каталог>`: снимки экранов без человека (S2-05) — агенты проверяют интерфейс по PNG, а не
//! управляют окном нажатиями.
//!
//! Сцены идут по очереди, как шаги замера (`drive_bench`): состояние ставится теми же методами, что и у
//! действий; затем ждём условие готовности сцены и ещё несколько показанных кадров; снимок — PrintWindow в
//! фоновом потоке; затем следующая сцена. На каждый снимок — строка `screen <имя> <путь> <ш>x<в>` в stdout,
//! в конце — выход с кодом 0; ошибка (снимок не удался, одного цвета, сцена не готова) — stderr и код 1.
//! Сообщения — по-английски: это диагностика для агентов, не текст интерфейса.
//!
//! Сцены (ADR 0005) — в описях частей (`PART.scenes`, модуль `scenes` части): имя, шаги `enter`/`ready`/`wait`,
//! `continues` (продолжает прошлую сцену своей части), `after` (встаёт за сценой другой части), `live`. Здесь —
//! двигатель: порядок (`order`), чистое начало, ожидание, снимок; сбросы «только в своей сцене» — `PART.reset`.
//!
//! `--screens <каталог> --live` (S3-04): живой ящик из окружения (`POSTMAN_URL`…) вместо демо — снимки `list`,
//! `letter` и `reply-inline` (S3-02, S5-06б) после первого `EmailsChanged` синхронизации, `calendar` и `event` — после недели с
//! сервера, `meeting` — после ответа о занятости участников из `POSTMAN_SCREENS_ATTENDEES` (адреса через `;`, S3-05),
//! `contacts` — после контактов и справочника с сервера (S4-02);
//! `vacation` и `rules` — после автоответа и правил с сервера (S4-05, S4-07), `shared` — «Входящие» общего ящика (S4-06);
//! `thread` (лента беседы, S5-05), `thread-view-menu`, `thread-stack`, `thread-list` (виды беседы, S5-05б), `lifecycle` и
//! `overdue` (сроки, S5-04), `search`, `search-letter`, `palette` и `palette-query` (поиск и палитра команд, S5-10),
//! `search-next` (F3 прокручивает к совпадению, S5-14) —
//! только демо;
//! вкладка «Настройки» (S5-09): `settings-general`, `settings-categories`, `settings-keys`, `settings-accounts` и первый
//! запуск `firstrun` — демо; `--live` без `POSTMAN_URL` — первый запуск (`firstrun`).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use gpui_kit::*;
use postman_cache::BodyRow;
use postman_core::bench;

use crate::list::MailList;
use crate::part::Part;
use crate::parts::PARTS;
use crate::{Mailer, Section, sys};

/// Кадров с выполненным условием до снимка: первые кадры после смены состояния ещё могут быть прежними.
const SETTLE_FRAMES: u32 = 3;
/// Снимок одного цвета (окно ещё не показано) — столько попыток; пауза перед второй, дальше — вдвое дольше
/// (50, 100, 200, 400, 800 мс: до ~1,5 с всего).
const CAPTURE_TRIES: u32 = 6;
const CAPTURE_BACKOFF: Duration = Duration::from_millis(50);
/// Сцена не готова за это время (воркер не ответил, страница не прочиталась) — ошибка.
const SCENE_TIMEOUT: Duration = Duration::from_secs(30);

/// Первая синхронизация живого ящика дольше сцены демо; обрыв связи в простое виден при опросе, раз в 2 мин.
const LIVE_TIMEOUT: Duration = Duration::from_secs(200);

/// Состояние сцены — через те же методы, что вызывают действия пользователя.
pub(crate) type Enter = fn(&Scene, &mut Mailer, &mut Window, &mut Context<Mailer>) -> Result<(), String>;
/// Условие готовности сцены: данные, которые она показывает, уже в состоянии view.
pub(crate) type Ready = fn(&Scene, &Mailer, &App) -> bool;
/// Шаг ожидания (перед проверкой готовности): то, что можно сделать только после фоновой работы.
pub(crate) type Wait = fn(&Scene, &mut Mailer, &mut Context<Mailer>);

/// Точка окна для ввода мышью (`Input`): обычно считается из модели — строка списка, ручка, кнопка.
pub(crate) type Place = fn(&Mailer, &Window, &App) -> Option<Point<Pixels>>;

/// Клавиша, зажатая при щелчке.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Held {
    No,
    Ctrl,
    Shift,
}

impl Held {
    /// `Ctrl` — главный модификатор щелчка: Ctrl, на macOS — ⌘ (`keymap::pick_click`, MAC-04).
    fn modifiers(self) -> Modifiers {
        let main = self == Held::Ctrl;
        let mac = crate::keymap::MAC;
        Modifiers { control: main && !mac, platform: main && mac, shift: self == Held::Shift, ..Modifiers::default() }
    }
}

/// Шаг ввода сцены: события идут только в это окно, внутри процесса (`Window::dispatch_event`), мимо ОС.
/// В чужое окно они попасть не могут, поэтому ввод разрешён на рабочей машине (решение владельца 2026-11-05).
#[derive(Clone, Copy)]
pub(crate) enum Input {
    /// Клавиши по одной, в записи gpui: `ctrl-n`, `shift-f3`, `down`. Символов в поле не вводят.
    Keys(&'static [&'static str]),
    /// Набор текста по символу.
    Text(&'static str),
    /// Набор текста, посчитанного во время прогона (даты относительно сегодня).
    Typed(fn() -> String),
    /// Дождаться условия. Не дождались за время сцены — ошибка прогона.
    Until(Ready),
    /// Навести мышь.
    Move(Place),
    /// Щелчок левой кнопкой.
    Click(Place, Held),
    /// Двойной щелчок левой кнопкой.
    Double(Place),
    /// Щелчок правой кнопкой.
    Right(Place),
    /// Колесо над точкой: на столько пикселей (вниз и вправо — плюс).
    Wheel(Place, f32),
    /// Перетащить левой кнопкой внутри окна.
    Drag(Place, Place),
    /// Зажать или отпустить модификатор (`KeyTips`).
    Hold(Held),
    /// Размер окна, логические пиксели.
    Resize(f32, f32),
    /// Шаг без ввода: то, что идёт мимо мыши и клавиш (диалог ОС, вкладка настроек с нужным разделом).
    Run(Enter),
    /// Щелчок по элементу с этим id (`spot`); сначала дождаться, пока он появится на экране.
    Press(&'static str),
}

/// Точки элементов для ввода (`Input::Press`): только в прогоне снимков, в обычной работе `spot` — пустой.
static SPOTS_ON: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Границы отмеченных элементов прошлого кадра по `ElementId` в записи `id-номер`.
    static SPOTS: RefCell<HashMap<String, Bounds<Pixels>>> = RefCell::default();
    /// Текст последней показанной подсказки.
    static TIP: RefCell<String> = RefCell::default();
}

/// Что прогон снимков не отдал в Windows (решение владельца 2026-11-06, п. 13): машина рабочая, сцены не пишут в её
/// буфер обмена и не показывают уведомления. Сцена сверяет, что было бы вызвано.
static HELD_CLIPBOARD: Mutex<Option<String>> = Mutex::new(None);
static HELD_NOTICES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Сцена просит считать окно неактивным: в прогоне оно бывает и так, и этак.
static IN_BACKGROUND: AtomicBool = AtomicBool::new(false);

/// Идёт прогон снимков (`--screens`).
pub(crate) fn screening() -> bool {
    SPOTS_ON.load(Ordering::Relaxed)
}

/// Текст в буфер обмена Windows; в прогоне снимков — только запомнить (`clipboard`).
pub(crate) fn to_clipboard(text: String, cx: &mut App) {
    if screening() {
        *HELD_CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
    } else {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// Что попало бы в буфер обмена.
pub(crate) fn clipboard() -> Option<String> {
    HELD_CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// HTML буфера обмена для вставки в письмо (T3-06); в прогоне снимков — образец сцены (`hold_clipboard_html`),
/// буфер Windows не читается.
pub(crate) fn clipboard_html(max: usize) -> Option<String> {
    if screening() { HELD_HTML.lock().unwrap_or_else(PoisonError::into_inner).clone() } else { crate::sys::clipboard_html(max) }
}

/// Сцена кладёт HTML «в буфер обмена» (Word, браузер) — вставка его прочтёт.
pub(crate) fn hold_clipboard_html(html: Option<String>) {
    *HELD_HTML.lock().unwrap_or_else(PoisonError::into_inner) = html;
}

static HELD_HTML: Mutex<Option<String>> = Mutex::new(None);

/// Уведомление Windows; в прогоне снимков — только запомнить (`notified`): AUMID в реестр не пишется.
pub(crate) fn notify(title: &str, body: &str) -> Result<(), String> {
    if screening() {
        HELD_NOTICES.lock().unwrap_or_else(PoisonError::into_inner).push((title.to_owned(), body.to_owned()));
        return Ok(());
    }
    // Щелчок по уведомлению возвращает окно из трея (T3-13; macOS — из скрытых, MAC-04).
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    return sys::shell::notify_click(title, body, None, || crate::tray::send(crate::tray::Signal::Open));
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    sys::shell::notify(title, body)
}

/// Уведомление Windows о новом письме (T2-10): `sound` — звук «Новое письмо», иначе молча.
pub(crate) fn notify_with(title: &str, body: &str, sound: bool) -> Result<(), String> {
    if screening() {
        HELD_NOTICES.lock().unwrap_or_else(PoisonError::into_inner).push((title.to_owned(), body.to_owned()));
        return Ok(());
    }
    sys::shell::notify_with(title, body, sound)
}

/// Уведомление о новых письмах (T3-13): щелчок открывает окно и письмо `letter` (`None` — «Входящие»).
pub(crate) fn notify_new_mail(title: &str, body: &str, sound: bool, letter: Option<String>) -> Result<(), String> {
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    if !screening() {
        return sys::shell::notify_click(title, body, Some(sound), move || crate::tray::send(crate::tray::Signal::Letter(letter.clone())));
    }
    let _ = letter;
    notify_with(title, body, sound)
}

/// Какие уведомления показал бы Windows.
pub(crate) fn notified() -> Vec<(String, String)> {
    HELD_NOTICES.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Окно активно: как в ОС, а в сцене с `in_background` — нет.
pub(crate) fn window_active(window: &Window) -> bool {
    window.is_window_active() && !IN_BACKGROUND.load(Ordering::Relaxed)
}

/// Сцена: считать окно неактивным (`window_active`).
pub(crate) fn in_background(on: bool) {
    IN_BACKGROUND.store(on, Ordering::Relaxed);
}

/// Отметка элемента для `Input::Press`: `.children(screens::spot(id))` внутри него. Границы — те же, что у строк
/// (`MailList::bounds`), только по имени: кнопок много, поле на каждую не заводится.
pub(crate) fn spot(id: impl Into<ElementId>) -> Option<impl IntoElement> {
    SPOTS_ON.load(Ordering::Relaxed).then(|| {
        let key = id.into().to_string();
        canvas(
            move |b, _, _| {
                SPOTS.with_borrow_mut(|s| {
                    s.insert(key, b);
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    })
}

/// Подсказка при наведении: как `Tooltip::new(text).build`, а в прогоне снимков ещё запоминает, что показано (`shown_tip`).
pub(crate) fn tip(text: String, window: &mut Window, cx: &mut App) -> AnyView {
    if SPOTS_ON.load(Ordering::Relaxed) {
        TIP.set(text.clone());
    }
    gpui_kit::component::tooltip::Tooltip::new(text).build(window, cx)
}

/// Последняя показанная подсказка (`tip`).
pub(crate) fn shown_tip() -> String {
    TIP.with_borrow(Clone::clone)
}

/// Забыть подсказку: следующий шаг ждёт новую.
pub(crate) fn forget_tip() {
    TIP.set(String::new());
}

/// Границы отмеченного элемента (`spot`).
pub(crate) fn spot_bounds(id: &str) -> Option<Bounds<Pixels>> {
    SPOTS.with_borrow(|s| s.get(id).copied())
}

/// Границы первого отмеченного элемента с такой приставкой id: номер у него не считается заранее.
pub(crate) fn spot_first(prefix: &str) -> Option<Bounds<Pixels>> {
    SPOTS.with_borrow(|s| s.iter().filter(|(k, _)| k.starts_with(prefix)).min_by_key(|(k, _)| (k.len(), (*k).clone())).map(|(_, b)| *b))
}

/// Отмеченные элементы `<приставка><номер>` сверху вниз: кнопки с той же приставкой не считаются.
pub(crate) fn spots_numbered(prefix: &str) -> Vec<Bounds<Pixels>> {
    let mut spots: Vec<Bounds<Pixels>> = SPOTS
        .with_borrow(|s| s.iter().filter(|(k, _)| k.strip_prefix(prefix).is_some_and(|n| n.parse::<usize>().is_ok())).map(|(_, b)| *b).collect());
    spots.sort_by(|a, b| f32::from(a.origin.y).total_cmp(&f32::from(b.origin.y)));
    spots
}

/// Показанная ширина картинки письма с такой шириной кадра (`preview::picture`).
pub(crate) fn picture_width(frame: u32) -> Option<f32> {
    spot_bounds(&format!("picture-{frame}")).map(|b| f32::from(b.size.width))
}

/// Середина отмеченного элемента: отметки живут один кадр, так что элемент виден сейчас.
pub(crate) fn spotted(id: &str) -> Option<Point<Pixels>> {
    spot_bounds(id).map(|b| b.center())
}

/// Забыть отметки посреди сцены: следующий кадр отметит только то, что на экране (шаг «элемента больше нет»).
pub(crate) fn forget_spots() {
    SPOTS.with_borrow_mut(HashMap::clear);
}

/// Оба элемента на экране и не пересекаются: стопка тостов не лежит на кнопке.
pub(crate) fn apart(a: &str, b: &str) -> bool {
    spot_bounds(a).zip(spot_bounds(b)).is_some_and(|(a, b)| !a.intersects(&b))
}

/// Показать стопку из трёх тостов с кнопками: сцены проверяют, что под ней и рядом с ней.
pub(crate) fn toast_stack(m: &mut Mailer, cx: &mut Context<Mailer>) {
    m.toasts.update(cx, |t, cx| {
        for n in 1..=3 {
            let undo = pets_ui::ToastAction::new(pets_i18n::tr!("bulk-undo"), |_, _| {});
            t.push_action(pets_ui::Level::Info, pets_i18n::tr!("bulk-done-archive", n = n), undo, cx);
        }
    });
}

/// Центр отмеченного элемента, если он внутри окна: иначе щелчок ушёл бы мимо.
pub(crate) fn spot_center(id: &str, window: &Window) -> Option<Point<Pixels>> {
    let view = window.viewport_size();
    spot_bounds(id).map(|b| b.center()).filter(|p| p.x > px(0.) && p.y > px(0.) && p.x < view.width && p.y < view.height)
}

/// Точка строки `i` списка с учётом заголовков групп: их число зависит от времени суток, строки демо считаются от «сейчас».
/// Строка за краем списка — `None`: щелчок попал бы в другую.
pub(crate) fn row_place(m: &Mailer, cx: &App, i: usize) -> Option<Point<Pixels>> {
    let list = m.list.read(cx);
    let b = list.bounds.get();
    // `row_point` прижимает точку к краю списка: на краю — уже не эта строка.
    (b.size.width > px(0.)).then(|| list.row_point(i)).flatten().filter(|p| p.y > b.origin.y && p.y < b.origin.y + b.size.height)
}

/// Событие очереди: по одному на кадр, чтобы окно успело перестроиться.
enum Event {
    Key(Keystroke),
    Raw(PlatformInput),
    Resize(Size<Pixels>),
    /// Пропустить кадр: отметки (`spot`) догонят то, что сделал прошлый шаг.
    Frame,
}

/// Сцена снимков (ADR 0005): значение в описи своей части (`PART.scenes`), шаги — функции части.
pub(crate) struct Scene {
    /// Имя файла (`<имя>.png`) и строки `screen …`.
    pub name: &'static str,
    /// Снимается и с `--live` (живой ящик стенда).
    pub live: bool,
    /// Встаёт за этой сценой другой части (по умолчанию — порядок `PARTS`); у остальных сцен части — следом.
    pub after: Option<&'static str>,
    /// Продолжает прошлую сцену своей части: её письмо, вкладку, меню, клавиши — без чистого начала.
    pub continues: bool,
    pub enter: Enter,
    pub ready: Ready,
    pub wait: Option<Wait>,
    /// Ввод сцены: идёт после `enter`, до проверки `ready`.
    pub input: &'static [Input],
    /// Только по имени в `POSTMAN_SCREENS_ONLY`: в обычный прогон снимков не входит.
    pub on_request: bool,
}

impl Scene {
    pub(crate) const fn new(name: &'static str, enter: Enter, ready: Ready) -> Self {
        Self { name, live: false, after: None, continues: false, enter, ready, wait: None, input: &[], on_request: false }
    }

    pub(crate) const fn live(self) -> Self {
        Self { live: true, ..self }
    }

    pub(crate) const fn after(self, scene: &'static str) -> Self {
        Self { after: Some(scene), ..self }
    }

    pub(crate) const fn continues(self) -> Self {
        Self { continues: true, ..self }
    }

    pub(crate) const fn wait(self, wait: Wait) -> Self {
        Self { wait: Some(wait), ..self }
    }

    pub(crate) const fn input(self, input: &'static [Input]) -> Self {
        Self { input, ..self }
    }

    pub(crate) const fn on_request(self) -> Self {
        Self { on_request: true, ..self }
    }
}

/// Все сцены по порядку съёмки: части — по `PARTS`, сцены части — подряд; сцена с `after` (и следующие за ней
/// сцены той же части) встаёт за названной сценой.
fn order(parts: &'static [Part]) -> Result<Vec<&'static Scene>, String> {
    let mut runs: Vec<(&str, Option<&str>, Vec<&'static Scene>)> = Vec::new();
    for p in parts {
        for (i, s) in p.scenes.iter().enumerate() {
            if i == 0 || s.after.is_some() {
                runs.push((p.name, s.after, Vec::new()));
            }
            if let Some((_, _, run)) = runs.last_mut() {
                run.push(s);
            }
        }
    }
    let mut all: Vec<&'static Scene> = Vec::new();
    let mut waiting = Vec::new();
    for (part, after, run) in runs {
        match after {
            None => all.extend(run),
            Some(_) => waiting.push((part, after, run)),
        }
    }
    while !waiting.is_empty() {
        let before = waiting.len();
        waiting.retain(|(_, after, run)| match all.iter().position(|s| Some(s.name) == *after) {
            Some(i) => {
                all.splice(i + 1..i + 1, run.iter().copied());
                false
            }
            None => true,
        });
        if waiting.len() == before {
            let names: Vec<String> = waiting.iter().map(|(p, a, _)| format!("{p}: {}", a.unwrap_or_default())).collect();
            return Err(format!("scenes after unknown scenes: {}", names.join(", ")));
        }
    }
    Ok(all)
}

/// Порядок сцен — один раз.
static ORDER: std::sync::LazyLock<Result<Vec<&'static Scene>, String>> = std::sync::LazyLock::new(|| order(PARTS));

/// Сцены по порядку; ошибка описей (`after` без такой сцены) — выход с кодом 1.
fn all_scenes() -> &'static [&'static Scene] {
    match &*ORDER {
        Ok(all) => all,
        Err(e) => {
            eprintln!("screens: {e}");
            finish(1)
        }
    }
}

#[derive(Clone, Copy)]
enum Phase {
    /// Поставить состояние сцены.
    Enter,
    /// Ждать готовности; `frames` — сколько кадров подряд условие выполнено.
    Wait { frames: u32, since: Instant },
    /// Снимок в фоне; кадры не запрашиваем.
    Capturing,
}

pub struct Driver {
    dir: PathBuf,
    scene: usize,
    phase: Phase,
    /// `--live`: живой ящик стенда вместо демо.
    pub(crate) live: bool,
    /// `--live` без учётной записи в окружении: снимок строки входа.
    signin: bool,
    /// Последнее письмо до сцены (`checks`, `reply-inline`): готово, когда открылось следующее.
    pub(crate) composer: Option<u64>,
    /// Окончание имён файлов: `-dark` для `--theme dark` (S5-01), `-gray` для `--theme gray` (T1-D), иначе пусто.
    suffix: &'static str,
    /// Сроки, поставленные сценой `lifecycle`: снять до следующих сцен.
    pub(crate) undo: Vec<postman_core::lifecycle::Patch>,
    /// Письмо с просроченным сроком (сцена `overdue`).
    pub(crate) overdue: Option<String>,
    /// Сцена `buttons`: поверх окна — образцы кнопок.
    pub(crate) gallery: bool,
    /// Следующий шаг `Scene::input`.
    input: usize,
    /// События текущего шага ввода.
    queue: VecDeque<Event>,
    /// Когда начат текущий шаг ввода.
    moved: Instant,
    /// Планировщик сроков работает и в этой сцене (`lifecycle_pass`): снимки без него повторяемы.
    pub(crate) planner: bool,
}

impl Driver {
    /// Показать образцы кнопок (сцена `buttons`).
    pub(crate) fn gallery(&self) -> bool {
        self.gallery
    }

    /// Каталог создаётся сразу: ошибка — до открытия окна.
    pub fn new(dir: PathBuf, live: bool, suffix: &'static str) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let dir = std::path::absolute(&dir)?;
        SPOTS_ON.store(true, Ordering::Relaxed);
        Ok(Self {
            dir,
            scene: 0,
            phase: Phase::Enter,
            live,
            signin: false,
            composer: None,
            suffix,
            input: 0,
            queue: VecDeque::new(),
            moved: Instant::now(),
            planner: false,
            undo: Vec::new(),
            overdue: None,
            gallery: false,
        })
    }

    /// Окно открывается со строкой входа: снимается она.
    pub fn sign_in(&mut self) {
        self.signin = true;
    }

    /// Сцены прогона: демо — все, `--live` — живые, без учётной записи — строка входа;
    /// `POSTMAN_SCREENS_ONLY=bulk,offline` — только эти (отладка сцен; сцена, которой нужны прежние, без них может
    /// не собраться).
    fn scenes(&self) -> Vec<&'static Scene> {
        let all = all_scenes().iter().copied().filter(|s| match (self.live, self.signin) {
            (_, true) => s.name == crate::signin::scenes::FIRSTRUN,
            (true, false) => s.live,
            (false, false) => true,
        });
        match std::env::var("POSTMAN_SCREENS_ONLY") {
            Ok(only) if !only.trim().is_empty() => all.filter(|s| only.split(',').any(|o| o.trim() == s.name)).collect(),
            _ => all.filter(|s| !s.on_request).collect(),
        }
    }

    fn timeout(&self) -> Duration {
        if self.live { LIVE_TIMEOUT } else { SCENE_TIMEOUT }
    }
}

/// Шаг сценария; вызывается из `Render for Mailer` на каждом кадре.
pub fn drive(m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) {
    let Some(d) = &m.screens else { return };
    let Some(&scene) = d.scenes().get(d.scene) else { return };
    let timeout = d.timeout();
    match d.phase {
        Phase::Capturing => return,
        Phase::Enter => {
            set_phase(m, Phase::Wait { frames: 0, since: Instant::now() });
            SPOTS.with(|s| s.borrow_mut().clear());
            if let Some(d) = &mut m.screens {
                d.input = 0;
                d.queue.clear();
            }
            if let Err(e) = enter(m, scene, window, cx) {
                fail(scene, &e);
            }
        }
        Phase::Wait { frames, since } => {
            if let Some(wait) = scene.wait {
                wait(scene, m, cx);
            }
            // Время — на шаг: длинный сценарий в отладочной сборке идёт дольше времени сцены, а не зависает.
            let since = m.screens.as_ref().map_or(since, |d| d.moved.max(since));
            if feed(m, scene, window, cx) {
                if since.elapsed() > timeout {
                    let at = m.screens.as_ref().map_or(0, |d| d.input);
                    if give_up(m, scene, &format!("input step {at} not done in {} s", timeout.as_secs()), window, cx) {
                        return;
                    }
                }
                set_phase(m, Phase::Wait { frames: 0, since });
            } else if !(scene.ready)(scene, m, cx) {
                if since.elapsed() > timeout && give_up(m, scene, &format!("scene not ready in {} s", timeout.as_secs()), window, cx) {
                    return;
                }
                set_phase(m, Phase::Wait { frames: 0, since });
            } else if frames + 1 < SETTLE_FRAMES {
                set_phase(m, Phase::Wait { frames: frames + 1, since });
            } else {
                set_phase(m, Phase::Capturing);
                capture(m, scene, window, cx);
                return;
            }
        }
    }
    // Границы — только этого кадра: от скрытого элемента не должно остаться точки.
    SPOTS.with_borrow_mut(HashMap::clear);
    // Кадры — по запросу (патч gpui): без запроса в простое ничего не рисуется.
    window.request_animation_frame();
    // Следующий кадр — целиком, без кэша частей: `ready` читает часть (`m.reading`) раньше её отрисовки, gpui не
    // засчитывает части её же сущность, и `notify` части теряется — снимок показал бы прошлое письмо.
    window.on_next_frame(|window, _| window.refresh());
}

/// Сцена не дошла: ошибка прогона. С `POSTMAN_SCREENS_SHOT_ON_FAIL` — сообщение и снимок как есть (разбор падения сценария).
fn give_up(m: &mut Mailer, scene: &'static Scene, e: &str, window: &Window, cx: &mut Context<Mailer>) -> bool {
    if std::env::var_os("POSTMAN_SCREENS_SHOT_ON_FAIL").is_none() {
        fail(scene, e);
    }
    eprintln!("screens: {}: {e}", scene.name);
    set_phase(m, Phase::Capturing);
    capture(m, scene, window, cx);
    true
}

/// Шаг ввода сцены; `true` — ввод ещё идёт. События уходят после кадра (`Window::defer`): кадр рисует `Mailer`,
/// а обработчики ввода меняют его же.
fn feed(m: &mut Mailer, scene: &'static Scene, window: &mut Window, cx: &mut Context<Mailer>) -> bool {
    let Some(d) = &mut m.screens else { return false };
    if let Some(e) = d.queue.pop_front() {
        window.defer(cx, move |window, cx| match e {
            Event::Key(k) => {
                window.dispatch_keystroke(k, cx);
            }
            Event::Raw(e) => {
                window.dispatch_event(e, cx);
            }
            Event::Resize(size) => window.resize(size),
            Event::Frame => {}
        });
        return true;
    }
    let Some(step) = scene.input.get(d.input).copied() else { return false };
    if let Input::Until(done) = step
        && !done(scene, m, cx)
    {
        return true;
    }
    if let Input::Press(id) = step
        && spot_center(id, window).is_none()
    {
        return true;
    }
    if let Input::Run(run) = step
        && let Err(e) = run(scene, m, window, cx)
    {
        fail(scene, &e);
    }
    let events = match events(step, m, window, cx) {
        Ok(events) => events,
        Err(e) => fail(scene, &e),
    };
    if let Some(d) = &mut m.screens {
        tracing::debug!("scene {}: step {}", scene.name, d.input);
        d.input += 1;
        d.queue = events;
        d.moved = Instant::now();
    }
    true
}

/// События шага ввода.
fn events(step: Input, m: &Mailer, window: &Window, cx: &App) -> Result<VecDeque<Event>, String> {
    let at = |place: Place| place(m, window, cx).ok_or_else(|| "no such place in the window".to_owned());
    let down = |position, button, held: Held| {
        Event::Raw(PlatformInput::MouseDown(MouseDownEvent { button, position, modifiers: held.modifiers(), click_count: 1, first_mouse: false }))
    };
    let up = |position, button, held: Held| {
        Event::Raw(PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers: held.modifiers(), click_count: 1 }))
    };
    let hover =
        |position, pressed_button| Event::Raw(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button, modifiers: Modifiers::default() }));
    let spot = |id: &str| spot_center(id, window).ok_or_else(|| format!("no element {id} in the window"));
    let mut out = VecDeque::new();
    match step {
        Input::Until(_) | Input::Run(_) => {}
        Input::Keys(keys) => {
            // Мимо `dispatch_keystroke`: он дописал бы к Enter ввод перевода строки, а поле отвечает на него `Change` — и строка
            // ошибки, поставленная по Enter, сразу снималась бы. Текст — шагом `Input::Text`.
            for k in keys {
                // Клавиши сцены — в записи таблицы команд (Windows); на macOS — как их привязала ОС (Ctrl → ⌘, MAC-04).
                let k = crate::keymap::for_os(k).unwrap_or_else(|| (*k).to_owned());
                let keystroke = Keystroke::parse_with_os(&k, cx.platform_info().os).map_err(|e| format!("key {k}: {e}"))?;
                out.push_back(Event::Raw(PlatformInput::KeyDown(KeyDownEvent { keystroke, is_held: false, prefer_character_input: false })));
            }
        }
        Input::Text(text) => typed(text, &mut out),
        Input::Typed(text) => typed(&text(), &mut out),
        Input::Move(place) => out.push_back(hover(at(place)?, None)),
        Input::Click(place, held) => {
            let p = at(place)?;
            out.extend([hover(p, None), down(p, MouseButton::Left, held), up(p, MouseButton::Left, held)]);
        }
        Input::Double(place) => {
            let p = at(place)?;
            let second = |e: Event| match e {
                Event::Raw(PlatformInput::MouseDown(e)) => Event::Raw(PlatformInput::MouseDown(MouseDownEvent { click_count: 2, ..e })),
                Event::Raw(PlatformInput::MouseUp(e)) => Event::Raw(PlatformInput::MouseUp(MouseUpEvent { click_count: 2, ..e })),
                e => e,
            };
            out.extend([hover(p, None), down(p, MouseButton::Left, Held::No), up(p, MouseButton::Left, Held::No)]);
            out.extend([second(down(p, MouseButton::Left, Held::No)), second(up(p, MouseButton::Left, Held::No))]);
        }
        Input::Right(place) => {
            let p = at(place)?;
            out.extend([hover(p, None), down(p, MouseButton::Right, Held::No), up(p, MouseButton::Right, Held::No)]);
        }
        Input::Wheel(place, by) => {
            let position = at(place)?;
            out.push_back(hover(position, None));
            out.push_back(Event::Raw(PlatformInput::Scroll(ScrollEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), px(-by))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
                source: ScrollSource::Pointer,
            })));
            // Отметки рисуются после этого шага: `Input::Press` сразу за прокруткой щёлкнул бы по прежнему месту элемента.
            out.extend([Event::Frame, Event::Frame]);
        }
        Input::Drag(from, to) => {
            let (a, b) = (at(from)?, at(to)?);
            let middle = point((a.x + b.x) / 2., (a.y + b.y) / 2.);
            out.extend([
                hover(a, None),
                down(a, MouseButton::Left, Held::No),
                hover(middle, Some(MouseButton::Left)),
                hover(b, Some(MouseButton::Left)),
                // Перетаскивание gpui начинается с первого движения: второе в точке `b` — уже его шаг.
                hover(b, Some(MouseButton::Left)),
                up(b, MouseButton::Left, Held::No),
            ]);
        }
        Input::Press(id) => {
            let p = spot(id)?;
            out.extend([hover(p, None), down(p, MouseButton::Left, Held::No), up(p, MouseButton::Left, Held::No)]);
        }
        Input::Hold(held) => out.push_back(Event::Raw(PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers: held.modifiers(),
            capslock: Capslock::default(),
        }))),
        Input::Resize(w, h) => out.push_back(Event::Resize(size(px(w), px(h)))),
    }
    Ok(out)
}

/// Нажатия для текста, по символу.
fn typed(text: &str, out: &mut VecDeque<Event>) {
    for c in text.chars() {
        let key_char = Some(c.to_string());
        let key = c.to_lowercase().to_string();
        out.push_back(Event::Key(Keystroke { modifiers: Modifiers::default(), key, key_char }));
    }
}

/// Чистое начало сцены: «Входящие» главной вкладки почты, без поиска, фильтров, выделения, меню и палитры.
fn clean_slate(m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) {
    m.close_menu(cx);
    m.close_picker(cx);
    crate::folders::scenes::close(m, cx);
    m.close_palette(window, cx);
    m.tabs.activate_kind(crate::tabs::TabKind::Main);
    m.set_section(Section::Mail, cx);
    if !m.search.read(cx).value().is_empty() {
        m.search.update(cx, |s, cx| s.set_value("", window, cx));
        m.run_search("", cx);
    }
    m.bar.update(cx, |b, _| b.scope = crate::bar::Scope::default());
    m.reset_filters(cx);
    m.filters.update(cx, |f, cx| {
        f.set(cx, |f| {
            f.panel = false;
            f.quick_open = false;
        });
        f.change(cx, |q| q.scope = crate::filters::Where::Folder);
    });
    m.list.update(cx, |l, cx| {
        l.clear_selection(cx);
    });
    m.reading.update(cx, |r, cx| r.clear(cx));
    if let Some(inbox) = crate::live::inbox(&m.folders.read(cx).mailboxes) {
        m.open_mailbox(inbox, cx);
    }
    // Список помнит прокрутку папки: строки считаются от начала, и сцена не зависит от прошлой.
    m.list.update(cx, |l, _| l.scroll.set_offset(Point::default()));
}

fn set_phase(m: &mut Mailer, phase: Phase) {
    if let Some(d) = &mut m.screens {
        d.phase = phase;
    }
}

/// Начало сцены: сбросы частей (своё состояние — только в своих сценах), чистый экран, затем шаг сцены.
fn enter(m: &mut Mailer, scene: &Scene, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    for reset in PARTS.iter().filter_map(|p| p.reset) {
        reset(scene, m, window, cx);
    }
    // Сцена начинается с чистого экрана: фильтры, выделение и модуль прошлой сцены ей не мешают (сцена
    // `delegation-mail` брала «Ждём» и «Отправленные» сцены `quick-wait`). Продолжения прошлой сцены — без этого.
    if !scene.continues {
        clean_slate(m, window, cx);
    }
    (scene.enter)(scene, m, window, cx)
}

/// Снимки идут по демо-ящику, а не по живому (`--live`).
pub(crate) fn demo(m: &Mailer) -> bool {
    m.screens.as_ref().is_some_and(|d| !d.live)
}

/// Снимки живого ящика (`--live`).
pub(crate) fn live(m: &Mailer) -> bool {
    m.screens.as_ref().is_some_and(|d| d.live)
}

/// Окно строк прочитано (первое — сразу при открытии папки, следующие — в фоне) и беседы разложены: до этого строки — письма
/// подряд, и номер строки указывает не туда.
pub(crate) fn rows_loaded(list: &MailList) -> bool {
    list.count > 0 && !list.rows.1.is_empty() && list.page_pending.is_none() && !list.computing
}

/// Открытое письмо области чтения.
pub(crate) fn reading<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a crate::reading::Reading> {
    m.reading.read(cx).reading.as_ref()
}

/// Ключ открытого письма области чтения.
pub(crate) fn reading_id(m: &Mailer, cx: &App) -> Option<String> {
    reading(m, cx).map(|r| r.row.id.clone())
}

/// Первое прочитанное письмо демо-ящика, тело которого подходит сцене.
pub(crate) fn demo_row(list: &MailList, wanted: impl Fn(&BodyRow) -> bool) -> Option<usize> {
    let (offset, rows) = &list.rows;
    rows.iter().position(|r| wanted(&postman_demo::body(&r.id))).map(|p| p + offset)
}

/// Id такого письма.
pub(crate) fn demo_id(list: &MailList, wanted: impl Fn(&BodyRow) -> bool) -> Option<&str> {
    demo_row(list, wanted).and_then(|i| list.row(i)).map(|r| r.id.as_str())
}

/// Выбрать такое письмо, как щелчок по строке.
pub(crate) fn select_demo(m: &mut Mailer, wanted: impl Fn(&BodyRow) -> bool, cx: &mut Context<Mailer>) -> Result<(), String> {
    let Some(i) = demo_row(m.list.read(cx), wanted) else {
        return Err("no such message in the first rows of the demo inbox".into());
    };
    m.list.update(cx, |l, cx| l.select(i, cx));
    Ok(())
}

/// У письма есть вложение демо (`postman_demo::blob`).
pub(crate) fn has(body: &BodyRow, blob: &str) -> bool {
    body.attachments.iter().any(|a| a.blob_id == blob)
}

/// Письмо — образец S/MIME демо (`postman_demo::smime`).
pub(crate) fn signed(body: &BodyRow, kind: &str) -> bool {
    body.blob_id.as_deref() == Some(format!("smime-{kind}").as_str())
}

/// Письмо Outlook демо: вложения для просмотра и картинка `cid:` в тексте.
pub(crate) fn outlook_letter(body: &BodyRow) -> bool {
    has(body, "pdf") && body.attachments.iter().any(|a| a.cid.is_some())
}

/// Вкладка «Настройки» открыта на разделе `section`.
pub(crate) fn settings_on(m: &Mailer, section: crate::settings_tab::Section, cx: &App) -> bool {
    m.tabs.is_active(crate::tabs::TabKind::Settings) && m.settings.view.as_ref().is_some_and(|v| v.read(cx).section == section)
}

/// PrintWindow — в фоновом потоке: он шлёт сообщения окну, и в потоке окна это повторный вход в оконную
/// процедуру gpui посреди отрисовки.
fn capture(m: &Mailer, scene: &'static Scene, window: &Window, cx: &mut Context<Mailer>) {
    let Some(d) = &m.screens else { return };
    let name = format!("{}{}", scene.name, d.suffix);
    let path = d.dir.join(format!("{name}.png"));
    let hwnd = sys::window_id(window).unwrap_or_else(|e| fail(scene, &e));
    let handle = window.window_handle();
    cx.spawn(async move |this, cx| {
        // Снимок одного цвета — окно ещё не нарисовано (первый прогон после сборки: шейдеры, первый показ окна):
        // попросить кадр и снять снова, с паузой вдвое дольше каждый раз. Обычно хватает первой попытки.
        let mut result = Err(Shot::Failed(String::new()));
        for attempt in 0..CAPTURE_TRIES {
            if attempt > 0 {
                let _ = cx.update_window(handle, |_, window, _| window.refresh());
                cx.background_executor().timer(CAPTURE_BACKOFF * (1 << (attempt - 1))).await;
            }
            let path = path.clone();
            result = cx.background_spawn(async move { shoot(hwnd, &path).map(|size| (path, size)) }).await;
            if !matches!(result, Err(Shot::Blank(_))) {
                break;
            }
        }
        let result = result.map_err(Shot::message);
        let _ = this.update(cx, |m, cx| {
            match result {
                Ok((path, (w, h))) => println!("screen {name} {} {w}x{h}", path.display()),
                Err(e) => fail(scene, &e),
            }
            let Some(d) = &mut m.screens else { return };
            d.scene += 1;
            d.phase = Phase::Enter;
            if d.scene >= d.scenes().len() {
                finish(0);
            }
            cx.notify();
        });
    })
    .detach();
}

/// Письмо в активной вкладке.
pub(crate) fn compose_tab(m: &Mailer) -> Option<&Entity<crate::composer::Composer>> {
    match m.tabs.active().kind {
        crate::tabs::TabKind::Compose(id) => m.compose.get(id),
        _ => None,
    }
}

/// Запомнить последнее письмо: сцена готова, когда откроется следующее.
pub(crate) fn remember_composer(m: &mut Mailer, cx: &App) {
    let last = m.compose.last().map(|c| c.read(cx).id);
    if let Some(d) = &mut m.screens {
        d.composer = last;
    }
}

/// Последнее письмо до сцены (`remember_composer`).
pub(crate) fn composer_before(m: &Mailer) -> Option<u64> {
    m.screens.as_ref().and_then(|d| d.composer)
}

/// Неудачный снимок: окно ещё не нарисовано (повторить) или ошибка.
enum Shot {
    Blank(String),
    Failed(String),
}

impl Shot {
    fn message(self) -> String {
        match self {
            Shot::Blank(e) | Shot::Failed(e) => e,
        }
    }
}

fn shoot(hwnd: isize, path: &Path) -> Result<(u32, u32), Shot> {
    let img = sys::capture(hwnd).map_err(Shot::Failed)?;
    if uniform(&img.rgba) {
        return Err(Shot::Blank(format!(
            "capture {}x{} is a single color after {CAPTURE_TRIES} tries: the window is not drawn",
            img.width, img.height
        )));
    }
    write_png(path, &img).map_err(|e| Shot::Failed(format!("{}: {e}", path.display())))?;
    Ok((img.width, img.height))
}

/// Все пиксели одного цвета (чёрный кадр, пустое окно).
fn uniform(rgba: &[u8]) -> bool {
    let (px, _) = rgba.as_chunks::<4>();
    px.iter().all(|p| Some(p) == px.first())
}

fn write_png(path: &Path, img: &sys::Image) -> Result<(), png::EncodingError> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    w.write_image_data(&img.rgba)?;
    w.finish()
}

fn fail(scene: &Scene, e: &str) -> ! {
    eprintln!("screens: {}: {e}", scene.name);
    finish(1)
}

/// Выход: рабочая копия ящика удаляется, как после замера (без `--keep`).
fn finish(code: i32) -> ! {
    bench::remove_work_copy();
    std::process::exit(code)
}

/// Событие окна для сцены (KeyTips — удержание Ctrl, подсказка — наведение): после кадра, когда экран почты не занят
/// отрисовкой; уходит в окно тем же путём, что ввод ОС.
pub(crate) fn send_input(window: &Window, cx: &mut Context<Mailer>, event: PlatformInput) {
    let handle = window.window_handle();
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| window.dispatch_event(event, cx));
    });
}

/// Нажатые модификаторы: только Ctrl (macOS — ⌘, MAC-04) или ничего.
pub(crate) fn ctrl(on: bool) -> ModifiersChangedEvent {
    let mac = crate::keymap::MAC;
    let modifiers = Modifiers { control: on && !mac, platform: on && mac, ..Modifiers::default() };
    ModifiersChangedEvent { modifiers, capslock: Capslock::default() }
}

#[cfg(test)]
mod tests {
    use super::uniform;

    #[test]
    fn one_color_capture_is_rejected() {
        assert!(uniform(&[0, 0, 0, 255].repeat(16)));
        assert!(uniform(&[]));
        let mut img = [250, 250, 250, 255].repeat(16);
        img[20] = 0;
        assert!(!uniform(&img));
    }

    /// Порядок сцен (ADR 0005): все сцены описей — по разу; продолжение прошлой сцены стоит сразу за сценой своей
    /// части — `after` чужой части не вклинился между ними.
    #[test]
    fn scene_order_keeps_continuations() {
        let all = super::order(crate::parts::PARTS).unwrap();
        let total: usize = crate::parts::PARTS.iter().map(|p| p.scenes.len()).sum();
        assert_eq!(all.len(), total);
        let part = |name: &str| crate::parts::PARTS.iter().find(|p| p.scenes.iter().any(|s| s.name == name)).map(|p| p.name);
        for w in all.windows(2) {
            if w[1].continues {
                assert_eq!(part(w[0].name), part(w[1].name), "{} продолжает {}", w[1].name, w[0].name);
            }
        }
    }
}
