//! Обновление Postman (решение 2026-11-11 «self-update», Postman#503) над общим крейтом Pets `pets-update`. Это
//! единственная сеть интерфейса по его собственной инициативе: проверка GitHub Releases (через минуту после старта и
//! дальше раз в час — крейт сам решает, прошли ли сутки) и скачивание — только по согласию («Установить»). Телеметрии
//! нет, в запросе только имя репозитория.
//!
//! - Первая строка `main` — [`helper`]: запуск копии exe помощником установки (`--pets-update-apply`).
//! - После первого окна — [`start`]: итог прошлой установки тостом («Обновлено до X» / «Не удалось обновить»),
//!   расписание проверок.
//! - Новая версия — тост «Вышла версия X · Установить»; «Установить» — скачивание с полосой хода и «Отменить»
//!   ([`Mailer::update_bar`]), проверка подписи, помощник установки и обычный выход (окно закрывается, помощник ставит
//!   версию и запускает Postman снова). MSI «для всех» сам спросит разрешение (UAC).
//! - «Настройки → Общие → Обновления»: выключатель «Проверять автоматически», кнопка «Проверить обновления»
//!   ([`CheckReason::Manual`]) и строка итога ([`View`]).
//!
//! Методы крейта блокирующие: каждая проверка и скачивание — свой поток, UI-поток только ждёт итог. Сети нет вовсе,
//! пока в сборке нет ключа подписи ([`UPDATE_PUBLIC_KEYS`]), при политике `AllowUpdate = 0` и в `--screens`/`--bench`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::{Level, ToastAction};
use pets_update::{
    Cancel, CheckOutcome, CheckReason, InstallKind, InstallPlan, Policy, Product, Settings, Update, UpdateError, UpdateResult, Updater,
    UreqTransport, Version,
};
use postman_core::prefs::{Channel, General};

use crate::Mailer;
use crate::migration::{link, meter, strip};
use crate::roles::Look;

/// Открытые ключи minisign выпусков (строка `RW…` из `.pub`; второй — на время смены ключа). Пусто — ни проверок,
/// ни скачиваний. Ключ общий с Урфином (id FBEB16F843D809C6, 2026-11-12); тот же — в переменной `UPDATE_PUBLIC_KEY`.
pub(crate) const UPDATE_PUBLIC_KEYS: &[&str] = &["RWTGCdhD+Bbr+0a0+iypW5+F7ZLUgkBqE3nPPC2N3zSxTkuUPSZ5E3gb"];

/// Файл-флаг portable рядом с exe: кладёт его архив (`package.ps1`) и `install.ps1`, MSI — нет.
pub(crate) const PORTABLE_FLAG: &str = "postman.portable";
/// Установки из архивов до флага: `install.ps1` лежит рядом с exe только у них (в MSI его нет).
const PORTABLE_SCRIPT: &str = "install.ps1";

/// Первая проверка по расписанию — через минуту после старта; дальше раз в час (крейт сам ждёт сутки с прошлой удачной
/// проверки и час после неудачной).
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
/// Как часто перерисовывать полосу хода скачивания.
const PROGRESS_TICK: Duration = Duration::from_millis(250);

/// Значок полосы скачивания (Segoe Fluent Icons «Download»).
pub(crate) const DOWNLOAD: char = '\u{E896}';

/// Продукт для крейта: репозиторий выпусков, имена артефактов (`postman-<версия>-windows-x64.msi|zip`), ключи.
pub(crate) fn product() -> Option<Product> {
    product_with(UPDATE_PUBLIC_KEYS)
}

fn product_with(keys: &'static [&'static str]) -> Option<Product> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
    Some(Product { name: "Postman", repo: "Dicobraz421/Postman", asset_prefix: "postman", current, public_keys: keys })
}

/// Первая строка `main`: запуск помощника установки — ставит обновление и выходит, окна нет.
pub(crate) fn helper() {
    if let Some(code) = product().and_then(|p| pets_update::run_helper(&p)) {
        std::process::exit(code);
    }
}

/// Как установлена программа: флаг portable или `install.ps1` рядом с exe — архив, иначе MSI.
pub(crate) fn install_kind(exe_dir: &Path) -> InstallKind {
    if [PORTABLE_FLAG, PORTABLE_SCRIPT].iter().any(|f| InstallKind::detect(exe_dir, f) == InstallKind::Portable) {
        InstallKind::Portable
    } else {
        InstallKind::Msi
    }
}

/// Что не перезаписывать при замене файлов portable. Профиль (`%LOCALAPPDATA%\Postman`) лежит не рядом с exe —
/// его замена не касается; остаётся флаг.
pub(crate) fn keep(kind: InstallKind) -> Vec<String> {
    match kind {
        InstallKind::Portable => vec![PORTABLE_FLAG.to_owned()],
        InstallKind::Msi => Vec::new(),
    }
}

/// Папка обновления в профиле: `%LOCALAPPDATA%\Postman\updates`.
pub(crate) fn dir(data: &Path) -> PathBuf {
    data.join("updates")
}

/// Настройки крейта из «Общих».
pub(crate) fn settings(g: &General) -> Settings {
    let channel = match g.channel {
        Channel::Stable => pets_update::Channel::Stable,
        Channel::Preview => pets_update::Channel::Beta,
    };
    Settings { enabled: g.auto_update, channel }
}

/// Политика: стандартная Pets (`HKLM\Software\Policies\Aahz\Postman`, `AllowUpdate` DWORD, читает крейт) и своя
/// Postman (`Software\Policies\Pets\Postman`, `AllowUpdate` строкой, как прочие политики Postman). Запрет любой — запрет.
pub(crate) fn policy(standard: Policy, own: Option<&str>) -> Policy {
    let own_off = own.map(str::trim).is_some_and(|v| ["0", "false", "off", "no"].iter().any(|n| v.eq_ignore_ascii_case(n)));
    if own_off { Policy { allow_update: Some(false) } } else { standard }
}

/// Что сейчас с обновлением — строка в «Настройках».
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    #[default]
    Idle,
    /// Идёт проверка по кнопке.
    Checking,
    /// Новее текущей версии ничего нет (по кнопке).
    UpToDate,
    /// Вышла версия.
    Available(String),
    /// Скачивается версия.
    Downloading(String),
    /// Подпись проверена, запущен помощник: окно закрывается.
    Installing(String),
    /// Проверка по кнопке не удалась: текст ошибки.
    Failed(String),
}

/// Ход скачивания.
#[derive(Clone)]
struct Download {
    version: String,
    received: Arc<AtomicU64>,
    total: u64,
    cancel: Cancel,
}

/// Состояние обновления — `Global` gpui: пишет только этот модуль, читают окно и «Настройки».
#[derive(Default)]
pub(crate) struct Model {
    /// `None` — без обновления (`--screens`, `--bench`, версия не semver): ни сети, ни файлов.
    updater: Option<Arc<Updater>>,
    /// Профиль: итог прошлой установки.
    dir: Option<PathBuf>,
    /// В сборке есть ключ подписи.
    keyed: bool,
    /// Политика не запрещает.
    allowed: bool,
    status: Status,
    /// Найденная версия: её ставит «Установить».
    offer: Option<Update>,
    download: Option<Download>,
    /// Идёт проверка (по кнопке или по расписанию).
    checking: bool,
    /// Как установлена программа (читается один раз, при запуске).
    kind: Option<InstallKind>,
}

impl Global for Model {}

/// Что видно в «Настройках».
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub(crate) allowed: bool,
    pub(crate) keyed: bool,
    pub(crate) status: Status,
    /// Можно нажать «Установить».
    pub(crate) offer: Option<String>,
    /// MSI: установка спросит UAC.
    pub(crate) msi: bool,
}

/// Модель для «Настроек»; до [`init`] (тесты) — по умолчанию.
pub(crate) fn view(cx: &App) -> View {
    let m = cx.try_global::<Model>();
    View {
        allowed: m.is_none_or(|m| m.allowed),
        keyed: m.is_some_and(|m| m.keyed),
        status: m.map(|m| m.status.clone()).unwrap_or_default(),
        offer: m.and_then(|m| m.offer.as_ref()).map(|u| u.version.to_string()),
        msi: m.is_some_and(|m| m.kind == Some(InstallKind::Msi)),
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default()
}

/// Поставить `Global`. `data` — каталог данных (`None` — `--screens`, `--bench`: без сети и файлов). Чтение политики —
/// микросекунды, сети нет.
pub(crate) fn init(data: Option<&Path>, cx: &mut App) {
    let kind = data.map(|_| install_kind(&exe_dir()));
    let updater = data.zip(product()).zip(kind).map(|((data, p), kind)| {
        let policy = policy(Policy::read(p.name), crate::sys::shell::policy("AllowUpdate").as_deref());
        Arc::new(Updater::with_parts(p, kind, dir(data), policy, Box::new(UreqTransport::new())))
    });
    let allowed = updater.as_ref().is_none_or(|u| u.policy().updates_allowed());
    cx.set_global(Model { updater, dir: data.map(dir), keyed: !UPDATE_PUBLIC_KEYS.is_empty(), allowed, kind, ..Model::default() });
}

fn edit(cx: &mut App, f: impl FnOnce(&mut Model)) {
    if cx.has_global::<Model>() {
        cx.update_global::<Model, _>(|m, _| f(m));
    }
}

/// Тост в окне (уведомления ставит `Mailer::new`); окна нет — только журнал.
fn toast(cx: &mut App, level: Level, text: String, action: Option<ToastAction>) {
    let Some(n) = cx.try_global::<crate::notices::Shared>().and_then(|w| w.0.upgrade()) else {
        tracing::info!("update: {text}");
        return;
    };
    n.update(cx, |t, cx| match action {
        Some(a) => t.push_action(level, text, a, cx),
        None => t.push(level, text, cx),
    });
}

/// Блокирующая работа крейта — в своём потоке; итог — в `done` на UI-потоке.
fn run<T: Send + 'static>(cx: &mut App, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T, &mut App) + 'static) {
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new().name("update".into()).spawn(move || {
        let _ = tx.send(work());
    });
    if let Err(e) = spawned {
        tracing::error!("update: поток обновления не запущен: {e}");
        return;
    }
    cx.spawn(async move |cx| {
        if let Ok(v) = rx.await {
            cx.update(|cx| done(v, cx));
        }
    })
    .detach();
}

/// После первого окна: итог прошлой установки и расписание проверок.
pub(crate) fn start(cx: &mut App) {
    let Some(m) = cx.try_global::<Model>() else { return };
    let (Some(dir), true) = (m.dir.clone(), m.updater.is_some()) else { return };
    let keyed = m.keyed;
    run(
        cx,
        move || product().and_then(|p| pets_update::take_result(&p, &dir)),
        |r, cx| {
            if let Some(r) = r {
                installed(&r, cx);
            }
        },
    );
    if !keyed {
        tracing::info!("update: в сборке нет ключа подписи, проверка обновлений выключена");
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            cx.update(scheduled);
            cx.background_executor().timer(CHECK_EVERY).await;
        }
    })
    .detach();
}

/// Тост по итогу прошлой установки.
fn installed(r: &UpdateResult, cx: &mut App) {
    if r.ok {
        tracing::info!(version = %r.version, "update: обновление поставлено");
        toast(cx, Level::Success, tr!("update-done", version = r.version.clone()), None);
    } else {
        let error = r.error.clone().unwrap_or_default();
        tracing::error!(version = %r.version, %error, "update: обновление не поставлено");
        toast(cx, Level::Error, tr!("update-failed", version = r.version.clone(), error = error), None);
    }
}

/// Проверка по расписанию: выключено настройкой — в крейт не идём, запроса нет.
fn scheduled(cx: &mut App) {
    if crate::prefs::get(cx).general.auto_update {
        spawn_check(CheckReason::Scheduled, cx);
    }
}

/// «Проверить обновления» (кнопка и команда `update.check`): сразу и при выключенной фоновой проверке.
pub(crate) fn check_now(cx: &mut App) {
    let v = view(cx);
    if !v.allowed {
        toast(cx, Level::Info, tr!("update-policy"), None);
        return;
    }
    if !v.keyed || cx.try_global::<Model>().is_none_or(|m| m.updater.is_none()) {
        toast(cx, Level::Info, tr!("update-no-key"), None);
        return;
    }
    spawn_check(CheckReason::Manual, cx);
}

fn spawn_check(reason: CheckReason, cx: &mut App) {
    let Some(m) = cx.try_global::<Model>() else { return };
    let busy = m.checking || m.download.is_some() || matches!(m.status, Status::Installing(_));
    let (Some(updater), true, false) = (m.updater.clone(), m.keyed && m.allowed, busy) else { return };
    let settings = settings(&crate::prefs::get(cx).general);
    edit(cx, |m| {
        m.checking = true;
        if reason == CheckReason::Manual {
            m.status = Status::Checking;
        }
    });
    run(cx, move || updater.check(&settings, reason), move |res, cx| checked(reason, res, cx));
}

/// Итог проверки: новая версия — тост «Установить»; по кнопке — и «последняя версия» или ошибка. Ошибки по
/// расписанию — только в журнал.
fn checked(reason: CheckReason, res: Result<CheckOutcome, UpdateError>, cx: &mut App) {
    let manual = reason == CheckReason::Manual;
    edit(cx, |m| m.checking = false);
    match res {
        Ok(CheckOutcome::Available(u)) => {
            let version = u.version.to_string();
            let known = cx.try_global::<Model>().and_then(|m| m.offer.as_ref()).is_some_and(|o| o.version == u.version);
            edit(cx, |m| {
                m.status = Status::Available(version.clone());
                m.offer = Some(u);
            });
            // Тост — один раз на версию; по кнопке — всегда.
            if manual || !known {
                offer_toast(&version, cx);
            }
        }
        Ok(CheckOutcome::UpToDate) if manual => {
            edit(cx, |m| m.status = Status::UpToDate);
            toast(cx, Level::Info, tr!("update-latest"), None);
        }
        Ok(CheckOutcome::Disabled(_)) if manual => toast(cx, Level::Info, tr!("update-policy"), None),
        Ok(_) => {}
        Err(e) if !manual => tracing::warn!(error = %e, "update: плановая проверка не удалась, повтор позже"),
        Err(e) => {
            let error = e.to_string();
            tracing::error!(%error, "update: проверка обновлений не удалась");
            edit(cx, |m| m.status = Status::Failed(error.clone()));
            toast(cx, Level::Warning, tr!("update-check-failed", error = error), None);
        }
    }
}

fn offer_toast(version: &str, cx: &mut App) {
    let action = ToastAction::new(tr!("update-install"), |_, cx| install(cx));
    toast(cx, Level::Info, tr!("update-available", version = version.to_owned()), Some(action));
}

/// «Установить»: скачать (полоса хода, «Отменить»), проверить подпись, запустить помощника и выйти обычным выходом.
pub(crate) fn install(cx: &mut App) {
    let Some(m) = cx.try_global::<Model>() else { return };
    if !m.allowed {
        toast(cx, Level::Info, tr!("update-policy"), None);
        return;
    }
    let (Some(updater), Some(update), None) = (m.updater.clone(), m.offer.clone(), m.download.as_ref()) else { return };
    if matches!(m.status, Status::Installing(_)) {
        return;
    }
    let version = update.version.to_string();
    let download = Download { version: version.clone(), received: Arc::default(), total: update.size, cancel: Cancel::new() };
    let (received, cancel) = (download.received.clone(), download.cancel.clone());
    edit(cx, |m| {
        m.status = Status::Downloading(version.clone());
        m.download = Some(download);
    });
    // Полоса хода перерисовывается, пока идёт скачивание.
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(PROGRESS_TICK).await;
            let going = cx.update(|cx| {
                let going = cx.try_global::<Model>().is_some_and(|m| m.download.is_some());
                if going {
                    edit(cx, |_| {});
                }
                going
            });
            if !going {
                return;
            }
        }
    })
    .detach();
    let kind = update.kind;
    run(
        cx,
        move || -> Result<(), UpdateError> {
            let got = updater.download(&update, &cancel, &mut |p| received.store(p.received, Ordering::Relaxed))?;
            let verified = updater.verify(got)?;
            let mut plan = InstallPlan::restart_current()?;
            plan.keep = keep(kind);
            updater.install(verified, plan)
        },
        move |res, cx| installed_now(&version, res, cx),
    );
}

/// Итог «Установить»: удача — окно закрывается (помощник ждёт выхода); отмена — тихо; ошибка — тост, предложение
/// остаётся.
fn installed_now(version: &str, res: Result<(), UpdateError>, cx: &mut App) {
    edit(cx, |m| m.download = None);
    match res {
        Ok(()) => {
            tracing::info!(%version, "update: установщик запущен, выход");
            edit(cx, |m| m.status = Status::Installing(version.to_owned()));
            cx.quit();
        }
        Err(UpdateError::Cancelled) => edit(cx, |m| m.status = Status::Available(version.to_owned())),
        Err(e) => {
            let error = e.to_string();
            tracing::error!(%version, %error, "update: обновление не поставлено");
            // Предложение остаётся: «Установить» можно нажать снова.
            edit(cx, |m| m.status = Status::Available(version.to_owned()));
            toast(cx, Level::Error, tr!("update-install-failed", error = error), None);
        }
    }
}

/// «Отменить» скачивание.
pub(crate) fn cancel(cx: &App) {
    if let Some(d) = cx.try_global::<Model>().and_then(|m| m.download.as_ref()) {
        d.cancel.cancel();
    }
}

/// Строка итога для «Настроек».
pub(crate) fn status_text(v: &View) -> String {
    if !v.keyed {
        return tr!("update-no-key");
    }
    match &v.status {
        Status::Idle => tr!("update-idle"),
        Status::Checking => tr!("update-checking"),
        Status::UpToDate => tr!("update-latest"),
        Status::Available(version) => tr!("update-available", version = version.clone()),
        Status::Downloading(version) => tr!("update-downloading", version = version.clone()),
        Status::Installing(version) if v.msi => format!("{} {}", tr!("update-installing", version = version.clone()), tr!("update-uac")),
        Status::Installing(version) => tr!("update-installing", version = version.clone()),
        Status::Failed(error) => tr!("update-check-failed", error = error.clone()),
    }
}

/// Целые мегабайты: получено — вниз, всего — вверх (как размер папки журнала в «Общих»).
fn mb(bytes: u64, up: bool) -> u64 {
    if up { bytes.div_ceil(1_048_576) } else { bytes / 1_048_576 }
}

impl Mailer {
    /// Полоса скачивания над списком: «Скачивается Postman X · 12,4 из 40,1 МБ · ▬▬ · Отменить».
    pub(crate) fn update_bar(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = cx.try_global::<Model>()?.download.clone()?;
        let done = d.received.load(Ordering::Relaxed);
        Some(
            strip("update-bar", DOWNLOAD, l)
                .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(tr!("update-downloading", version = d.version)))
                .child(div().flex_none().text_color(l.text2()).child(tr!(
                    "update-progress",
                    done = mb(done, false) as i64,
                    total = mb(d.total, true) as i64
                )))
                .child(meter(done, d.total, 160., false, l))
                .child(div().flex_1())
                .child(link("update-cancel", tr!("update-cancel"), l, cx, |_, cx| cancel(cx)))
                .into_any_element(),
        )
    }
}

/// Опись части (ADR 0005, `parts.rs`): команды, тексты и сцены обновления.
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "update", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, reset: Some(scenes::reset) };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/update.ftl")), ("en", include_str!("../i18n/en/update.ftl"))];

/// Действия: «Проверить обновления», «Установить обновление».
pub(crate) mod act {
    gpui_kit::actions!(update, [Check, Install]);
}

const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAILER, cmd};
    &[cmd("update.check", &[], MAILER), cmd("update.install", &[], MAILER)]
};

fn handlers(root: Div, cx: &mut Context<Mailer>) -> Div {
    root.on_action(cx.listener(|_, _: &act::Check, _, cx| check_now(cx))).on_action(cx.listener(|_, _: &act::Install, _, cx| install(cx)))
}

/// Сцены `--screens`: тост «Вышла версия», полоса скачивания, раздел «Обновления» в «Общих». Сети нет: модель
/// заполняется образцом.
pub(crate) mod scenes {
    use gpui_kit::*;

    use super::{Download, Model, Status};
    use crate::screens::Scene;
    use crate::settings_tab::Section as S;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Тост «Вышла версия 0.2.0 · Установить» над строкой состояния.
        Scene::new("update-available", available, available_ready),
        // Идёт скачивание: полоса «Скачивается Postman 0.2.0 · 12,4 из 40,0 МБ · Отменить».
        Scene::new("update-downloading", downloading, downloading_ready),
        // «Настройки → Общие», прокручены вниз: «Обновления» — канал, «Проверять автоматически», «Проверить обновления»,
        // строка итога «Вышла версия 0.2.0» и «Установить».
        Scene::new("settings-updates", settings, settings_ready),
    ];

    const VERSION: &str = "0.2.0";

    pub(crate) fn reset(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        let Some(model) = cx.try_global::<Model>() else { return };
        if model.status == Status::Idle && model.offer.is_none() && model.download.is_none() {
            return;
        }
        // Своя прошлая сцена: тост «Вышла версия» в следующие не переходит.
        m.toasts.update(cx, |t, cx| t.hide_all(cx));
        cx.update_global::<Model, _>(|m, _| {
            m.status = Status::Idle;
            m.download = None;
            m.offer = None;
            m.keyed = false;
        });
    }

    fn sample(m: &mut Model) {
        m.keyed = true;
        m.allowed = true;
        m.status = Status::Available(VERSION.into());
        let asset = format!("postman-{VERSION}-windows-x64.msi");
        let base = format!("https://github.com/Dicobraz421/Postman/releases/download/v{VERSION}");
        m.offer = Some(pets_update::Update {
            version: pets_update::Version::new(0, 2, 0),
            kind: pets_update::InstallKind::Msi,
            page_url: format!("https://github.com/Dicobraz421/Postman/releases/tag/v{VERSION}"),
            asset_url: format!("{base}/{asset}"),
            size: 40 << 20,
            signature_url: format!("{base}/{asset}.minisig"),
            asset_name: asset,
        });
    }

    fn available(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        super::edit(cx, sample);
        super::offer_toast(VERSION, cx);
        Ok(())
    }

    fn available_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.toasts.read(cx).toast_shown()
    }

    fn downloading(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        super::edit(cx, |m| {
            sample(m);
            m.status = Status::Downloading(VERSION.into());
            let d = Download { version: VERSION.into(), received: Default::default(), total: 40 << 20, cancel: Default::default() };
            d.received.store(13_002_342, std::sync::atomic::Ordering::Relaxed);
            m.download = Some(d);
        });
        cx.notify();
        Ok(())
    }

    fn downloading_ready(_: &Scene, _: &Mailer, cx: &App) -> bool {
        cx.try_global::<Model>().is_some_and(|m| m.download.is_some())
    }

    fn settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        super::edit(cx, sample);
        m.open_settings(Some(S::General), window, cx);
        if let Some(v) = m.settings.view.clone() {
            v.update(cx, |v, cx| {
                v.scroll.scroll_to_bottom();
                cx.notify();
            });
        }
        Ok(())
    }

    fn settings_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        crate::screens::settings_on(m, S::General, cx)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Mutex;

    use std::path::Path;
    use std::sync::Arc;

    use pets_update::{ARCH, CheckOutcome, CheckReason, DisabledBy, InstallKind, Policy, Response, Settings, Transport, UpdateError, Updater};
    use postman_core::prefs::{Channel, General};

    use super::{PORTABLE_FLAG, PORTABLE_SCRIPT, dir, install_kind, keep, policy, product_with, settings};

    const KEY: &[&str] = &["RWQtest"];

    /// Сеть: на любой запрос — список выпусков с `v{version}` (MSI, архив и их подписи); журнал запросов.
    #[derive(Clone)]
    struct Net {
        version: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Net {
        fn new(version: &'static str) -> Net {
            Net { version, calls: Arc::default() }
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl Transport for Net {
        fn get(&self, url: &str) -> Result<Response, UpdateError> {
            self.calls.lock().unwrap().push(url.to_owned());
            let v = self.version;
            let asset = |ext: &str| {
                let name = format!("postman-{v}-windows-{ARCH}.{ext}");
                format!(
                    r#"{{"name":"{name}","browser_download_url":"https://example.invalid/{name}","size":10}},
                    {{"name":"{name}.minisig","browser_download_url":"https://example.invalid/{name}.minisig","size":1}}"#
                )
            };
            let body = format!(
                r#"[{{"tag_name":"v{v}","prerelease":{},"html_url":"https://github.com/Dicobraz421/Postman/releases/tag/v{v}",
                "assets":[{},{}]}}]"#,
                v.contains('-'),
                asset("msi"),
                asset("zip")
            );
            Ok(Response { len: None, body: Box::new(Cursor::new(body.into_bytes())) })
        }
    }

    fn updater(kind: InstallKind, policy: Policy, net: &Net, dir: &Path) -> Updater {
        Updater::with_parts(product_with(KEY).unwrap(), kind, dir, policy, Box::new(net.clone()))
    }

    fn on() -> Settings {
        settings(&General { auto_update: true, channel: Channel::Stable, ..General::default() })
    }

    /// Новая версия: MSI-установка получает `.msi`, portable — `.zip`; имена — по шаблону выпуска (`package.ps1`).
    #[test]
    fn update_available_picks_msi_or_zip() {
        for (kind, ext) in [(InstallKind::Msi, "msi"), (InstallKind::Portable, "zip")] {
            let dir = tempfile::tempdir().unwrap();
            let net = Net::new("99.0.0");
            let got = updater(kind, Policy::default(), &net, dir.path()).check(&on(), CheckReason::Scheduled).unwrap();
            let CheckOutcome::Available(u) = got else { panic!("{kind:?}: {got:?}") };
            assert_eq!(u.version.to_string(), "99.0.0");
            assert_eq!(u.kind, kind);
            assert_eq!(u.asset_name, format!("postman-99.0.0-windows-{ARCH}.{ext}"));
            assert_eq!(u.signature_url, format!("https://example.invalid/postman-99.0.0-windows-{ARCH}.{ext}.minisig"));
            assert_eq!(net.calls(), 1);
            assert!(net.calls.lock().unwrap()[0].starts_with("https://api.github.com/repos/Dicobraz421/Postman/releases"));
        }
    }

    /// Нет новее (та же версия) — `UpToDate`; по расписанию второй запрос в те же сутки не идёт.
    #[test]
    fn update_none_and_daily_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new(env!("CARGO_PKG_VERSION"));
        let u = updater(InstallKind::Msi, Policy::default(), &net, dir.path());
        assert_eq!(u.check(&on(), CheckReason::Scheduled).unwrap(), CheckOutcome::UpToDate);
        assert_eq!(u.check(&on(), CheckReason::Scheduled).unwrap(), CheckOutcome::NotDue);
        assert_eq!(net.calls(), 1);
        // Кнопка — сразу.
        assert_eq!(u.check(&on(), CheckReason::Manual).unwrap(), CheckOutcome::UpToDate);
        assert_eq!(net.calls(), 2);
    }

    /// Политика `AllowUpdate = 0` (стандартная или своя Postman) — ни одного запроса, и по кнопке тоже; выключенная
    /// настройка — нет фоновых, кнопка работает.
    #[test]
    fn update_policy_and_setting_off() {
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new("99.0.0");
        for p in [Policy::from_values(Some(0), None), policy(Policy::default(), Some("0"))] {
            let u = updater(InstallKind::Msi, p, &net, dir.path());
            assert!(!u.policy().updates_allowed());
            for reason in [CheckReason::Scheduled, CheckReason::Manual] {
                assert_eq!(u.check(&on(), reason).unwrap(), CheckOutcome::Disabled(DisabledBy::Policy));
            }
        }
        assert_eq!(net.calls(), 0);
        let off = settings(&General { auto_update: false, ..General::default() });
        let u = updater(InstallKind::Msi, Policy::default(), &net, dir.path());
        assert_eq!(u.check(&off, CheckReason::Scheduled).unwrap(), CheckOutcome::Disabled(DisabledBy::Settings));
        assert_eq!(net.calls(), 0);
        assert!(matches!(u.check(&off, CheckReason::Manual).unwrap(), CheckOutcome::Available(_)));
        assert_eq!(net.calls(), 1);
    }

    /// Канал: стабильный не видит бету, предварительный — видит.
    #[test]
    fn update_channel_preview_sees_beta() {
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new("99.0.0-beta.1");
        let u = updater(InstallKind::Msi, Policy::default(), &net, dir.path());
        assert_eq!(u.check(&on(), CheckReason::Manual).unwrap(), CheckOutcome::UpToDate);
        let preview = settings(&General { channel: Channel::Preview, ..General::default() });
        assert!(matches!(u.check(&preview, CheckReason::Manual).unwrap(), CheckOutcome::Available(_)));
    }

    /// Вид установки: флаг или `install.ps1` рядом с exe — portable (флаг не перезаписывается), иначе MSI.
    #[test]
    fn update_install_kind_and_keep() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(install_kind(dir.path()), InstallKind::Msi);
        assert!(keep(InstallKind::Msi).is_empty());
        std::fs::write(dir.path().join(PORTABLE_SCRIPT), "").unwrap();
        assert_eq!(install_kind(dir.path()), InstallKind::Portable);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PORTABLE_FLAG), "").unwrap();
        assert_eq!(install_kind(dir.path()), InstallKind::Portable);
        assert_eq!(keep(InstallKind::Portable), [PORTABLE_FLAG]);
    }

    /// Своя политика Postman — строкой; пусто или «1» — как стандартная.
    #[test]
    fn update_own_policy_values() {
        for off in ["0", " false ", "OFF"] {
            assert!(!policy(Policy::default(), Some(off)).updates_allowed(), "{off}");
        }
        assert!(policy(Policy::default(), Some("1")).updates_allowed());
        assert!(policy(Policy::default(), None).updates_allowed());
        assert!(!policy(Policy::from_values(Some(0), None), Some("1")).updates_allowed());
    }

    #[test]
    fn update_product_and_dir() {
        let p = product_with(KEY).unwrap();
        assert_eq!((p.name, p.repo, p.asset_prefix), ("Postman", "Dicobraz421/Postman", "postman"));
        assert_eq!(p.current.to_string(), env!("CARGO_PKG_VERSION"));
        let data = Path::new("Postman");
        assert_eq!(dir(data), data.join("updates"));
    }
}
