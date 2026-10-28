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
//! `thread` (лента беседы, S5-05), `folder-menu`, `thread-stack`, `thread-list` (виды беседы, S5-05б), `lifecycle` и
//! `overdue` (сроки, S5-04), `search`, `search-letter`, `palette` и `palette-query` (поиск и палитра команд, S5-10),
//! `search-next` (F3 прокручивает к совпадению, S5-14) —
//! только демо;
//! вкладка «Настройки» (S5-09): `settings-general`, `settings-categories`, `settings-keys`, `settings-accounts` и первый
//! запуск `firstrun` — демо; `--live` без `POSTMAN_URL` — первый запуск (`firstrun`).

use std::path::{Path, PathBuf};
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

/// Первая синхронизация живого ящика дольше сцены демо.
const LIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// Состояние сцены — через те же методы, что вызывают действия пользователя.
pub(crate) type Enter = fn(&Scene, &mut Mailer, &mut Window, &mut Context<Mailer>) -> Result<(), String>;
/// Условие готовности сцены: данные, которые она показывает, уже в состоянии view.
pub(crate) type Ready = fn(&Scene, &Mailer, &App) -> bool;
/// Шаг ожидания (перед проверкой готовности): то, что можно сделать только после фоновой работы.
pub(crate) type Wait = fn(&Scene, &mut Mailer, &mut Context<Mailer>);

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
}

impl Scene {
    pub(crate) const fn new(name: &'static str, enter: Enter, ready: Ready) -> Self {
        Self { name, live: false, after: None, continues: false, enter, ready, wait: None }
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
        Ok(Self { dir, scene: 0, phase: Phase::Enter, live, signin: false, composer: None, suffix, undo: Vec::new(), overdue: None, gallery: false })
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
            _ => all.collect(),
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
            if let Err(e) = enter(m, scene, window, cx) {
                fail(scene, &e);
            }
        }
        Phase::Wait { frames, since } => {
            if let Some(wait) = scene.wait {
                wait(scene, m, cx);
            }
            if !(scene.ready)(scene, m, cx) {
                if since.elapsed() > timeout {
                    fail(scene, &format!("scene not ready in {} s", timeout.as_secs()));
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
    // Кадры — по запросу (патч gpui): без запроса в простое ничего не рисуется.
    window.request_animation_frame();
    // Следующий кадр — целиком, без кэша частей: `ready` читает часть (`m.reading`) раньше её отрисовки, gpui не
    // засчитывает части её же сущность, и `notify` части теряется — снимок показал бы прошлое письмо.
    window.on_next_frame(|window, _| window.refresh());
}

/// Чистое начало сцены: «Входящие» главной вкладки почты, без поиска, фильтров, выделения, меню и палитры.
fn clean_slate(m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) {
    m.close_menu(cx);
    m.close_picker(cx);
    m.close_folder_menu(cx);
    m.close_palette(window, cx);
    m.tabs.activate_kind(crate::tabs::TabKind::Main);
    m.set_section(Section::Mail, cx);
    if !m.search.read(cx).value().is_empty() {
        m.search.update(cx, |s, cx| s.set_value("", window, cx));
        m.run_search("", cx);
    }
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

/// Окно строк прочитано (первое — сразу при открытии папки, следующие — в фоне).
pub(crate) fn rows_loaded(list: &MailList) -> bool {
    list.count > 0 && !list.rows.1.is_empty() && list.page_pending.is_none()
}

/// Открытое письмо области чтения.
pub(crate) fn reading<'a>(m: &'a Mailer, cx: &'a App) -> Option<&'a crate::reading::Reading> {
    m.reading.read(cx).reading.as_ref()
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

/// Нажатые модификаторы: только Ctrl или ничего.
pub(crate) fn ctrl(on: bool) -> ModifiersChangedEvent {
    ModifiersChangedEvent { modifiers: Modifiers { control: on, ..Modifiers::default() }, capslock: Capslock::default() }
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
