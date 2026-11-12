//! Значки Fluent UI System Icons, Regular 20 (ADR 0008, DESIGN «Значки»): файлы `res/icons/`, вшиты в exe, источник
//! активов gpui — [`Assets`], рисует [`icon`]. Остальные значки — шрифтом ОС (`chrome::icon`), кнопки окна — всегда им;
//! на macOS шрифта Segoe Fluent Icons нет — те же значки SVG Lucide из gpui-kit (MAC-02, [`mac_glyph`]).
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
    ARCHIVE = "archive",
    MOVE = "move",
    TAG = "tag",
    FLAG = "flag",
    CLOCK = "clock",
    SEARCH = "search",
    VIEW = "view",
    SYNC = "arrow-sync",
    // Дерево папок.
    INBOX = "inbox",
    SEND = "send",
    DRAFTS = "drafts",
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
        #[cfg(target_os = "macos")]
        if path.starts_with("icons/") {
            return MacGlyphs.load(path);
        }
        Ok(SET.iter().find(|(p, _)| *p == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        #[cfg_attr(not(target_os = "macos"), expect(unused_mut))]
        let mut list: Vec<SharedString> = SET.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect();
        #[cfg(target_os = "macos")]
        list.extend(MacGlyphs.list(path)?);
        Ok(list)
    }
}

/// Значок набора размером `size` и цветом `color`.
pub(crate) fn icon(ico: Ico, size: f32, color: Hsla) -> Svg {
    svg().path(ico.0).flex_none().size(px(size)).text_color(color)
}

// macOS: значки шрифта Segoe Fluent Icons (`chrome::icon`) — встроенными SVG Lucide (gpui-kit-assets, ISC): шрифта
// на Mac нет. Вшиты только эти; путь `icons/…` — тот же, что у компонентов gpui-kit (значки те же).
#[cfg(target_os = "macos")]
gpui_kit::assets::icon_assets!(pub(crate) MacGlyphs, [
    Archive, ArrowDown, ArrowDownUp, ArrowLeft, ArrowUp, BadgeCheck, Ban, BellRing, Bold, BookOpen, Calendar,
    Calendar1, CalendarCheck, CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight, ChevronUp, Circle,
    CircleCheck, CircleQuestionMark, CircleX, Clock, ClockArrowLeft, Copy, Download, Ellipsis, Eraser, ExternalLink,
    Eye, EyeOff, FileOutput, FileText, Flag, Folder, FolderInput, Forward, Globe, Hourglass, Image, ImagePlay, Info,
    Italic, Keyboard, Layers, Link, List, ListChecks, ListFilter, ListTodo, Lock, Mail, MailOpen, MailWarning, MapPin,
    MessagesSquare, Minus, Monitor, NotebookPen, Palette, Paperclip, Pause, Plane, Play, Plus, Puzzle, Quote,
    RefreshCw, Repeat, Reply, ReplyAll, Save, Search, Send, Settings, Shield, ShieldAlert, Signature,
    SlidersHorizontal, Square, SquarePen, Table, Tag, Timer, Trash, TriangleAlert, Underline, Undo2, Upload, User,
    UserPlus, Users, Video, WifiOff, X, ZoomIn, ZoomOut,
]);

/// SVG для значка шрифта Segoe Fluent Icons; нет пары — `None` (значок рисуется шрифтом).
#[cfg(target_os = "macos")]
pub(crate) fn mac_glyph(g: char) -> Option<&'static str> {
    Some(match g {
        '\u{E121}' => "icons/clock.svg",
        '\u{E707}' => "icons/map-pin.svg",
        '\u{E70B}' => "icons/notebook-pen.svg",
        '\u{E70D}' => "icons/chevron-down.svg",
        '\u{E70E}' => "icons/chevron-up.svg",
        '\u{E70F}' => "icons/square-pen.svg",
        '\u{E710}' => "icons/plus.svg",
        '\u{E711}' => "icons/x.svg",
        '\u{E712}' => "icons/ellipsis.svg",
        '\u{E713}' => "icons/settings.svg",
        '\u{E714}' => "icons/video.svg",
        '\u{E715}' => "icons/mail.svg",
        '\u{E716}' => "icons/users.svg",
        '\u{E71B}' => "icons/link.svg",
        '\u{E71C}' => "icons/list-filter.svg",
        '\u{E71F}' => "icons/zoom-out.svg",
        '\u{E721}' => "icons/search.svg",
        '\u{E723}' => "icons/paperclip.svg",
        '\u{E724}' => "icons/send.svg",
        '\u{E72B}' => "icons/arrow-left.svg",
        '\u{E72C}' => "icons/refresh-cw.svg",
        '\u{E72E}' => "icons/lock.svg",
        '\u{E730}' => "icons/shield-alert.svg",
        '\u{E733}' => "icons/ban.svg",
        '\u{E738}' => "icons/minus.svg",
        '\u{E73E}' => "icons/check.svg",
        '\u{E74A}' => "icons/arrow-up.svg",
        '\u{E74B}' => "icons/arrow-down.svg",
        '\u{E74D}' => "icons/trash.svg",
        '\u{E74E}' => "icons/save.svg",
        '\u{E762}' => "icons/list-checks.svg",
        '\u{E765}' => "icons/keyboard.svg",
        '\u{E768}' => "icons/play.svg",
        '\u{E769}' => "icons/pause.svg",
        '\u{E76B}' => "icons/chevron-left.svg",
        '\u{E76C}' => "icons/chevron-right.svg",
        '\u{E771}' => "icons/sliders-horizontal.svg",
        '\u{E774}' => "icons/globe.svg",
        '\u{E77B}' => "icons/user.svg",
        '\u{E787}' => "icons/calendar.svg",
        '\u{E790}' => "icons/palette.svg",
        '\u{E7A7}' => "icons/undo-2.svg",
        '\u{E7B3}' => "icons/eye.svg",
        '\u{E7B8}' => "icons/archive.svg",
        '\u{E7BA}' => "icons/triangle-alert.svg",
        '\u{E7C1}' => "icons/flag.svg",
        '\u{E7C4}' => "icons/layers.svg",
        '\u{E7F4}' => "icons/monitor.svg",
        '\u{E81C}' => "icons/clock-arrow-left.svg",
        '\u{E823}' => "icons/timer.svg",
        '\u{E894}' => "icons/eraser.svg",
        '\u{E895}' => "icons/refresh-cw.svg",
        '\u{E896}' => "icons/download.svg",
        '\u{E897}' => "icons/circle-question-mark.svg",
        '\u{E898}' => "icons/upload.svg",
        '\u{E89C}' => "icons/forward.svg",
        '\u{E8A1}' => "icons/book-open.svg",
        '\u{E8A3}' => "icons/zoom-in.svg",
        '\u{E8A5}' => "icons/file-text.svg",
        '\u{E8A7}' => "icons/external-link.svg",
        '\u{E8A9}' => "icons/table.svg",
        '\u{E8AC}' => "icons/signature.svg",
        '\u{E8B3}' => "icons/list-checks.svg",
        '\u{E8B7}' => "icons/folder.svg",
        '\u{E8B9}' => "icons/image.svg",
        '\u{E8BB}' => "icons/x.svg",
        '\u{E8BD}' => "icons/plane.svg",
        '\u{E8BF}' => "icons/calendar-1.svg",
        '\u{E8C0}' => "icons/calendar-days.svg",
        '\u{E8C2}' => "icons/reply-all.svg",
        '\u{E8C3}' => "icons/mail-open.svg",
        '\u{E8C8}' => "icons/copy.svg",
        '\u{E8CB}' => "icons/arrow-down-up.svg",
        '\u{E8D1}' => "icons/calendar-check.svg",
        '\u{E8D8}' => "icons/mail-warning.svg",
        '\u{E8DB}' => "icons/italic.svg",
        '\u{E8DC}' => "icons/underline.svg",
        '\u{E8DD}' => "icons/bold.svg",
        '\u{E8DE}' => "icons/folder-input.svg",
        '\u{E8EC}' => "icons/tag.svg",
        '\u{E8EE}' => "icons/repeat.svg",
        '\u{E8F2}' => "icons/messages-square.svg",
        '\u{E8FA}' => "icons/user-plus.svg",
        '\u{E8FD}' => "icons/list.svg",
        '\u{E916}' => "icons/hourglass.svg",
        '\u{E921}' => "icons/minus.svg",
        '\u{E922}' => "icons/square.svg",
        '\u{E923}' => "icons/copy.svg",
        '\u{E930}' => "icons/circle-check.svg",
        '\u{E946}' => "icons/info.svg",
        '\u{E97A}' => "icons/reply.svg",
        '\u{E9B2}' => "icons/quote.svg",
        '\u{E9D5}' => "icons/list-todo.svg",
        '\u{EA18}' => "icons/shield.svg",
        '\u{EA39}' => "icons/circle-x.svg",
        '\u{EA3A}' => "icons/circle.svg",
        '\u{EA86}' => "icons/puzzle.svg",
        '\u{EA8F}' => "icons/bell-ring.svg",
        '\u{EB95}' => "icons/badge-check.svg",
        '\u{EB9F}' => "icons/image-play.svg",
        '\u{ED1A}' => "icons/eye-off.svg",
        '\u{EDE1}' => "icons/file-output.svg",
        '\u{F384}' => "icons/wifi-off.svg",
        _ => return None,
    })
}

/// macOS: значок шрифта, нарисованный SVG ([`mac_glyph`]) цветом текста родителя — `svg()` своего цвета не наследует,
/// а цвет текста к моменту `render` уже задан предками (`chrome::icon(…).text_color(…)`).
#[cfg(target_os = "macos")]
#[derive(IntoElement)]
pub(crate) struct Tinted {
    pub(crate) path: &'static str,
    pub(crate) size: f32,
}

#[cfg(target_os = "macos")]
impl RenderOnce for Tinted {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        svg().path(self.path).flex_none().size(px(self.size)).text_color(window.text_style().color)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource;

    use super::{APP, Assets, COMPOSE, Ico, SEARCH, SET, SETTINGS, VIEW};

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
        // На macOS под `icons/` — значки вместо шрифта (`MacGlyphs`), это значки самих компонентов gpui-kit.
        #[cfg(not(target_os = "macos"))]
        assert!(Assets.load("icons/search.svg").unwrap().is_none());
        assert_eq!(Assets.list("fluent/").unwrap().len(), SET.len());
    }

    /// macOS: у каждого значка шрифта в коде интерфейса — SVG во вшитом наборе (иначе на Mac был бы «тофу»).
    #[cfg(target_os = "macos")]
    #[test]
    fn every_font_glyph_has_an_svg_on_macos() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing = Vec::new();
        let mut dirs = vec![dir];
        while let Some(d) = dirs.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for part in source.split("'\\u{").skip(1) {
                    let hex: String = part.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                    let Some(g) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) else { continue };
                    // Частная область Unicode (значки шрифта); границы — числами, чтобы тест не нашёл сам себя.
                    if (0xE000..=0xF8FF).contains(&u32::from(g)) {
                        match super::mac_glyph(g) {
                            Some(p) => assert!(Assets.load(p).unwrap().is_some(), "{p}"),
                            None => missing.push(format!("{}: U+{hex}", path.display())),
                        }
                    }
                }
            }
        }
        assert!(missing.is_empty(), "значки без SVG на macOS: {missing:?}");
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
            assert!(has(b.ico), "{}", b.cmd);
        }
        assert!(has(COMPOSE) && has(SEARCH) && has(VIEW));
        for role in [None, Some("inbox"), Some("sent"), Some("drafts"), Some("trash"), Some("archive"), Some("junk"), Some("other")] {
            assert!(has(crate::folders::folder_icon(role).0), "{role:?}");
        }
    }
}
