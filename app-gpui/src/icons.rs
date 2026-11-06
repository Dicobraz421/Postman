//! Значки Fluent UI System Icons, Regular 20 (ADR 0008, DESIGN «Значки»): файлы `res/icons/`, вшиты в exe, источник
//! активов gpui — [`Assets`], рисует [`icon`]. Остальные значки — шрифтом ОС (`chrome::icon`), кнопки окна — всегда им.
//!
//! Пути активов — `fluent/…`, не `icons/…`: под `icons/` компоненты gpui-kit ищут свои значки, и совпавшее имя появилось
//! бы в них само. `svg()` берёт цвет только из своего стиля, не наследует: цвет — обязательный параметр [`icon`].

use std::borrow::Cow;

use gpui_kit::*;

/// Значок набора: путь в [`Assets`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ico(&'static str);

/// Значки набора: имя и файл `res/icons/<файл>.svg`.
macro_rules! set {
    ($($name:ident = $file:literal),* $(,)?) => {
        $(pub(crate) const $name: Ico = Ico(concat!("fluent/", $file, ".svg"));)*
        const SET: &[(&str, &[u8])] = &[
            $((concat!("fluent/", $file, ".svg"), include_bytes!(concat!("../res/icons/", $file, ".svg")))),*
        ];
    };
}

set! {
    // Разделы и шестерёнка заголовка окна.
    MAIL = "mail",
    CALENDAR = "calendar",
    PEOPLE = "people",
    TASKS = "tasks",
    SETTINGS = "settings",
    // Командная строка.
    COMPOSE = "compose",
    REPLY = "reply",
    REPLY_ALL = "reply-all",
    FORWARD = "forward",
    DELETE = "delete",
    FLAG = "flag",
    SEARCH = "search",
    // Дерево папок.
    INBOX = "inbox",
    SEND = "send",
    DRAFTS = "drafts",
    ARCHIVE = "archive",
    JUNK = "junk",
    FOLDER = "folder",
}

/// Значок приложения «Ленты» в заголовке окна: многоцветный, рисуется `img`, не `svg()` (тот красит одним цветом).
/// Размер в файле — 20 px: `img` растрирует SVG по размеру из файла.
pub(crate) const APP: &str = "fluent/postman.svg";
const APP_BYTES: &[u8] = include_bytes!("../res/postman.svg");

/// Вшитые значки — источник активов gpui.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == APP {
            return Ok(Some(Cow::Borrowed(APP_BYTES)));
        }
        Ok(SET.iter().find(|(p, _)| *p == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(SET.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect())
    }
}

/// Значок набора размером `size` и цветом `color`.
pub(crate) fn icon(ico: Ico, size: f32, color: Hsla) -> Svg {
    svg().path(ico.0).flex_none().size(px(size)).text_color(color)
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource;

    use super::{APP, Assets, COMPOSE, Ico, SEARCH, SET, SETTINGS};

    /// Файлы каталога и таблица совпадают, у каждого — запись в `README.md`, контур и сетка 20 × 20.
    #[test]
    fn set_matches_the_folder() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("res/icons");
        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
        let mut files: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".svg")).collect();
        files.sort();
        let mut set: Vec<String> = SET.iter().map(|(p, _)| p.trim_start_matches("fluent/").to_owned()).collect();
        set.sort();
        assert_eq!(files, set, "файлы res/icons и таблица `set!`");
        for (path, bytes) in SET {
            let text = std::str::from_utf8(bytes).unwrap();
            assert!(text.contains(r#"viewBox="0 0 20 20""#) && text.contains("<path d="), "{path}");
            let name = path.trim_start_matches("fluent/");
            assert!(readme.contains(&format!("`{name}`")), "{name} нет в README.md");
        }
    }

    /// Активы: значок приложения и каждый значок находятся, чужой путь — нет (компоненты gpui-kit живут без него).
    #[test]
    fn assets_serve_the_set() {
        assert!(Assets.load(APP).unwrap().is_some());
        for (path, _) in SET {
            assert!(Assets.load(path).unwrap().is_some(), "{path}");
        }
        assert!(Assets.load("icons/search.svg").unwrap().is_none());
        assert_eq!(Assets.list("fluent/").unwrap().len(), SET.len());
    }

    /// У каждой команды со значком Fluent и у каждой роли папки — файл в наборе.
    #[test]
    fn every_command_icon_is_in_the_set() {
        let has = |i: Ico| SET.iter().any(|(p, _)| *p == i.0);
        for s in crate::chrome::SECTIONS {
            assert!(has(s.icon), "{}", s.command);
        }
        assert!(has(SETTINGS));
        for b in crate::bar::BUTTONS {
            assert!(has(b.0), "{}", b.3);
        }
        assert!(has(COMPOSE) && has(SEARCH));
        for role in [None, Some("inbox"), Some("sent"), Some("drafts"), Some("trash"), Some("archive"), Some("junk"), Some("other")] {
            assert!(has(crate::folders::folder_icon(role).0), "{role:?}");
        }
    }
}
