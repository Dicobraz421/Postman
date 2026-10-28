//! Настройки пользователя в интерфейсе (S5-09): `postman_core::prefs::Prefs` — `Global` gpui. Файл читается до
//! первого окна (`load`, микросекунды), пишется в фоне после каждой правки (`update`); `--bench` и `--screens` его
//! не читают и не пишут (снимки повторяемы). Тема, плотность и язык применяются здесь же, на лету.

use std::path::{Path, PathBuf};

use gpui_kit::{App, BorrowAppContext as _, Global};
use pets_ui::theme::{Density, ThemeError, Themes};
use postman_core::prefs::{self, Prefs, Writer};

struct Store {
    prefs: Prefs,
    /// `None` — не сохранять (замер, снимки).
    path: Option<PathBuf>,
    writer: Writer,
}

impl Global for Store {}

/// Путь файла настроек в каталоге данных.
pub(crate) fn path(dir: &Path) -> PathBuf {
    dir.join(prefs::FILE)
}

/// Прочитать настройки при запуске. Файл испорчен или новее этой версии — по умолчанию, причина — в журнал
/// (файл не перезаписывается до первой правки).
/// Свои клавиши — к текущей ревизии реестра (`keymap::migrate`).
pub(crate) fn load(dir: &Path) -> Prefs {
    let mut p = match Prefs::load(&path(dir)) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("{e}");
            Prefs::default()
        }
    };
    crate::keymap::migrate(crate::commands::all(), &mut p.keys, p.keys_rev);
    p.keys_rev = crate::keymap::KEYS_REV;
    p
}

/// Поставить `Global`; `path` — куда писать (`None` — не писать).
pub(crate) fn init(prefs: Prefs, path: Option<PathBuf>, cx: &mut App) {
    cx.set_global(Store { prefs, path, writer: Writer::default() });
}

/// Текущие настройки; до [`init`] (тесты) — по умолчанию.
pub(crate) fn get(cx: &App) -> Prefs {
    cx.try_global::<Store>().map(|s| s.prefs.clone()).unwrap_or_default()
}

/// Часть настроек без копии всего (строки списка читают её на каждой строке).
pub(crate) fn with<R>(cx: &App, f: impl FnOnce(&Prefs) -> R) -> R {
    match cx.try_global::<Store>() {
        Some(s) => f(&s.prefs),
        None => f(&Prefs::default()),
    }
}

/// Перерисовать view `T`, когда настройки изменились (раскладка экрана почты зависит от «Панели чтения»).
pub(crate) fn observe<T: 'static>(cx: &mut gpui_kit::Context<T>) -> gpui_kit::Subscription {
    cx.observe_global::<Store>(|_, cx| cx.notify())
}

/// Настройки изменились — `f` у view `T` (экран почты снимает фильтры скрытых закладок, S5-03).
pub(crate) fn observe_with<T: 'static>(
    cx: &mut gpui_kit::Context<T>,
    f: impl Fn(&mut T, &mut gpui_kit::Context<T>) + 'static,
) -> gpui_kit::Subscription {
    cx.observe_global::<Store>(move |this, cx| f(this, cx))
}

/// Изменить настройки: запись — в фоне; изменились свои клавиши — привязки заново.
pub(crate) fn update(cx: &mut App, f: impl FnOnce(&mut Prefs)) {
    if !cx.has_global::<Store>() {
        init(Prefs::default(), None, cx);
    }
    let (before, after, path, writer) = cx.update_global::<Store, _>(|s, _| {
        let before = s.prefs.clone();
        f(&mut s.prefs);
        s.prefs = std::mem::take(&mut s.prefs).normalized();
        (before, s.prefs.clone(), s.path.clone(), s.writer.clone())
    });
    if before == after {
        return;
    }
    if before.keys != after.keys || before.keys_single != after.keys_single || before.key_scheme != after.key_scheme {
        crate::commands::bind(cx, &after);
    } else if before.keytips != after.keytips {
        crate::keytips::rebuild(cx, &after);
    }
    let Some(path) = path else { return };
    let seq = writer.next();
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = writer.write(seq, || after.save(&path)) {
                tracing::warn!("{e}");
            }
        })
        .detach();
}

/// Тема: выбор pets-ui и в настройки. Политика запретила смену — ошибка, настройки не меняются.
pub(crate) fn set_theme(id: &str, cx: &mut App) -> Result<(), ThemeError> {
    Themes::set_choice(id, cx)?;
    let id = id.to_owned();
    update(cx, |p| p.general.theme = Some(id));
    // «Серая» (T1-D) — слой токенов Postman, а не тема pets-ui: заново под выбранную.
    let p = get(cx);
    crate::roles::set_accent(p.look.accent, density_at_start(None, &p), cx);
    Ok(())
}

/// Плотность (высота строки, px): действует и при политике темы.
pub(crate) fn set_density(row: u16, cx: &mut App) {
    let density = Density::from_row(i64::from(row));
    Themes::update(cx, |s| s.set_density(density));
    update(cx, |p| p.general.density = density.map(|d| d.row()));
}

/// Акцент («Внешний вид», S5-30): тема — заново, на лету.
pub(crate) fn set_accent(accent: postman_core::prefs::Accent, cx: &mut App) {
    update(cx, |p| p.look.accent = accent);
    let density = density_at_start(None, &get(cx));
    crate::roles::set_accent(accent, density, cx);
}

/// Язык интерфейса на лету (`None` — язык Windows): тексты, которые экраны строят при отрисовке, — сразу; подписи,
/// запомненные полями ввода, — после перезапуска.
pub(crate) fn set_language(lang: Option<&str>, cx: &mut App) {
    for p in pets_i18n::set_language(lang.unwrap_or("auto")) {
        tracing::warn!("i18n: {p}");
    }
    let lang = lang.map(str::to_owned);
    update(cx, |p| p.general.language = lang);
    cx.refresh_windows();
}

/// Тема при запуске: `--theme`, иначе из настроек, иначе «как в ОС».
pub(crate) fn theme_at_start(arg: Option<&str>, prefs: &Prefs) -> String {
    match arg {
        Some(_) => crate::roles::choice(arg).to_owned(),
        None => prefs.general.theme.clone().unwrap_or_else(|| pets_ui::theme::SYSTEM.to_owned()),
    }
}

/// Плотность при запуске: `--density`, иначе из настроек.
pub(crate) fn density_at_start(arg: Option<&str>, prefs: &Prefs) -> Option<Density> {
    crate::roles::density(arg).or_else(|| prefs.general.density.and_then(|d| Density::from_row(i64::from(d))))
}

/// Язык при запуске: `--lang`, иначе из настроек, иначе язык Windows.
pub(crate) fn language_at_start(arg: Option<&str>, prefs: &Prefs) -> String {
    arg.map(str::to_owned).or_else(|| prefs.general.language.clone()).unwrap_or_else(|| "auto".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Аргументы запуска сильнее настроек; без них — настройки.
    #[test]
    fn start_values_prefer_arguments() {
        let mut p = Prefs::default();
        p.general.theme = Some("fluent-dark".into());
        p.general.density = Some(20);
        p.general.language = Some("en".into());
        assert_eq!(theme_at_start(None, &p), "fluent-dark");
        assert_eq!(theme_at_start(Some("light"), &p), pets_ui::theme::FLUENT_LIGHT);
        assert_eq!(density_at_start(None, &p), Some(Density::Compact));
        assert_eq!(density_at_start(Some("compact"), &p), Some(Density::Compact));
        assert_eq!(language_at_start(None, &p), "en");
        assert_eq!(language_at_start(Some("ru"), &p), "ru");
        let d = Prefs::default();
        assert_eq!(theme_at_start(None, &d), pets_ui::theme::SYSTEM);
        assert_eq!(language_at_start(None, &d), "auto");
    }
}
