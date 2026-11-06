//! Сценарии с вводом (`Scene::input`): разделы `docs/MANUAL-TESTS.md`, где нужны клавиши и мышь. Ввод идёт внутри процесса,
//! `ready` сцены — ожидаемый итог шага: не дождались, и прогон падает с именем сцены. Снимок остаётся для просмотра.
//!
//! В обычный прогон снимков сцены не входят (`on_request`): запускаются по именам из `app-gpui/tests/manual.rs`.

use gpui_kit::*;
use pets_i18n::tr;
use postman_core::model::Features;

use crate::screens::{Held, Input, Scene, rows_loaded};
use crate::tabs::TabKind;
use crate::{Mailer, Section};

pub(crate) const PART: crate::part::Part = crate::part::Part { name: "drill", scenes: SCENES, ..crate::part::Part::NONE };

const SCENES: &[Scene] = &[
    // Раздел 22, шаг 5: Ctrl+N дважды, Ctrl+Tab, Ctrl+W — вкладки закрываются, «Почта» остаётся.
    Scene::new("drill-tabs", start, only_main).input(TABS).on_request(),
    // Раздел 29, шаги 2, 7: Ctrl+щелчок, Shift+щелчок, правая кнопка, Esc снимает выделение.
    Scene::new("drill-select", start, none_selected).input(SELECT).on_request(),
    // T1-05б: перетаскивание разделителя меняет ширину списка.
    Scene::new("drill-split", start, list_resized).input(SPLIT).on_request(),
    // Раздел 39, шаг 1: удержание Ctrl показывает плашки, отпускание убирает.
    Scene::new("drill-keytips", start, no_tips).input(TIPS).on_request(),
    // Раздел 30, шаги 1, 3: Ctrl+K, запрос кириллицей, совпавшие буквы, Esc закрывает.
    Scene::new("drill-palette", start, palette_closed).input(PALETTE).on_request(),
    // S7-05: сервер только с почтой — в заголовке одна «Почта», разделитель и шестерёнка на месте.
    Scene::new("drill-nav-mail-only", start, only_mail_section).input(NAV_MAIL_ONLY).on_request(),
    // T1-26: разделы и шестерёнка щелчком, подсказка, выход из вкладки письма; зоны заголовка идут встык с кнопками;
    // одиннадцать вкладок в узком окне не наезжают на шестерёнку и «─ □ ✕». Последняя: окно остаётся узким.
    Scene::new("drill-nav", start, title_fits).input(NAV).on_request(),
];

const TABS: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-n"]),
    Input::Until(one_letter),
    Input::Keys(&["ctrl-n"]),
    Input::Until(two_letters),
    Input::Keys(&["ctrl-tab"]),
    Input::Until(main_active),
    Input::Keys(&["ctrl-w"]),
    Input::Until(two_letters),
    Input::Keys(&["ctrl-tab", "ctrl-w"]),
    Input::Until(one_letter),
    Input::Keys(&["ctrl-tab"]),
    Input::Until(letter_active),
    Input::Keys(&["ctrl-w"]),
];

const SELECT: &[Input] = &[
    Input::Until(mail_ready),
    Input::Move(row2),
    Input::Click(row0, Held::No),
    Input::Click(row1, Held::Ctrl),
    Input::Until(two_selected),
    Input::Click(row3, Held::Shift),
    Input::Until(range_selected),
    Input::Right(row1),
    Input::Until(menu_open),
    Input::Keys(&["escape"]),
    Input::Until(menu_closed),
    Input::Keys(&["escape"]),
];

const SPLIT: &[Input] = &[Input::Until(mail_ready), Input::Until(bounds_known), Input::Drag(split, split_left)];

const TIPS: &[Input] = &[Input::Until(mail_ready), Input::Hold(Held::Ctrl), Input::Until(tips), Input::Hold(Held::No)];

const PALETTE: &[Input] = &[
    Input::Until(mail_ready),
    Input::Keys(&["ctrl-k"]),
    Input::Until(palette_open),
    Input::Text("арх"), // i18n-ok: ввод сценария
    Input::Until(palette_matched),
    Input::Keys(&["escape"]),
];

const NAV_MAIL_ONLY: &[Input] = &[Input::Until(mail_ready), Input::Run(mail_only_server), Input::Until(title_tiled)];

const NAV: &[Input] = &[
    Input::Until(mail_ready),
    Input::Until(title_tiled),
    Input::Press("nav-calendar"),
    Input::Until(|_, m, _| m.section == Section::Calendar),
    Input::Press("nav-contacts"),
    Input::Until(|_, m, _| m.section == Section::Contacts),
    Input::Press("nav-tasks"),
    Input::Until(|_, m, _| m.section == Section::Tasks),
    Input::Press("nav-settings"),
    Input::Until(|_, m, _| m.tabs.is_active(TabKind::Settings)),
    Input::Press("nav-mail"),
    Input::Until(|_, m, _| m.section == Section::Mail && m.tabs.is_active(TabKind::Main)),
    Input::Run(|_, _, _, _| {
        crate::screens::forget_tip();
        Ok(())
    }),
    Input::Move(|_, _, _| crate::screens::spotted("nav-calendar")),
    Input::Until(|_, _, cx| {
        crate::screens::shown_tip() == crate::keytips::with_key(tr!("frame-section-calendar"), crate::keytips::key("view.calendar", cx))
    }),
    Input::Keys(&["ctrl-n"]),
    Input::Until(one_letter),
    Input::Press("nav-calendar"),
    Input::Until(|_, m, _| m.section == Section::Calendar && m.tabs.is_active(TabKind::Main) && letters(m) == 1),
    Input::Press("nav-mail"),
    Input::Until(|_, m, _| m.section == Section::Mail),
    Input::Keys(&["ctrl-t", "ctrl-t", "ctrl-t", "ctrl-t", "ctrl-t", "ctrl-t", "ctrl-t", "ctrl-t"]),
    Input::Until(|_, m, _| m.tabs.iter().count() > 10),
    Input::Resize(760., 800.),
];

/// Заголовок слева направо: кнопки и зоны перетаскивания идут встык, друг на друга не заходят. Зона — сосед кнопки, а
/// не родитель (`chrome.rs`), поэтому щелчок по кнопке не двигает окно, а зазор между кнопками — двигает.
fn title_tiled(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let has = |id: &str| crate::chrome::SECTIONS.iter().any(|s| s.id == id && crate::features::available(s.command, crate::features::get(cx)));
    let left: Vec<String> = ["nav-mail", "nav-calendar", "nav-contacts", "nav-tasks"]
        .into_iter()
        .filter(|id| has(id))
        .enumerate()
        .flat_map(|(i, id)| [(i > 0).then(|| format!("nav-gap-{i}")), Some(id.to_owned())])
        .flatten()
        .collect();
    let chain = |ids: &[&str]| {
        let bounds: Option<Vec<_>> = ids.iter().map(|id| crate::screens::spot_bounds(id)).collect();
        bounds.is_some_and(|b| b.windows(2).all(|p| (p[0].right() - p[1].left()).abs() < px(0.5)))
    };
    let mut ids = vec!["app-icon"];
    ids.extend(left.iter().map(String::as_str));
    ids.extend(["nav-end", "nav-tabs"]);
    let height = crate::screens::spot_bounds("drag").is_some_and(|b| b.size.height == px(crate::chrome::TITLEBAR_H));
    m.tabs.is_active(TabKind::Main) && height && chain(&ids) && chain(&["drag", "nav-settings", "caption-min", "caption-max", "caption-close"])
}

fn mail_only_server(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
    crate::screens::forget_spots();
    m.set_features(Features::NONE.with(postman_core::model::Feature::Keywords, true), cx);
    Ok(())
}

fn only_mail_section(s: &Scene, m: &Mailer, cx: &App) -> bool {
    let gone = ["nav-calendar", "nav-contacts", "nav-tasks", "nav-gap-1"].into_iter().all(|id| crate::screens::spot_bounds(id).is_none());
    gone && title_tiled(s, m, cx) && crate::screens::spot_bounds("nav-mail").is_some()
}

/// Узкое окно, одиннадцать вкладок: `min_w` зоны перетаскивания держится, справа всё на месте, разделы — на своём.
fn title_fits(_: &Scene, m: &Mailer, _: &App) -> bool {
    let spot = crate::screens::spot_bounds;
    let (Some(drag), Some(gear), Some(close), Some(mail)) = (spot("drag"), spot("nav-settings"), spot("caption-close"), spot("nav-mail")) else {
        return false;
    };
    let room = (drag.right() - gear.left()).abs() < px(0.5) && drag.size.width >= px(48.);
    m.tabs.iter().count() > 10 && close.right() <= px(760.) && room && (mail.left() - px(46.)).abs() < px(1.)
}

/// Строка списка.
fn row(m: &Mailer, cx: &App, i: usize) -> Option<Point<Pixels>> {
    crate::screens::row_place(m, cx, i)
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

/// Разделитель справа от списка.
fn split(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    let b = m.list.read(cx).bounds.get();
    Some(point(b.origin.x + b.size.width + px(SPLIT_GAP), b.origin.y + b.size.height / 2.))
}

fn split_left(m: &Mailer, w: &Window, cx: &App) -> Option<Point<Pixels>> {
    split(m, w, cx).map(|p| point(p.x - px(SPLIT_MOVE), p.y))
}

/// Писем в двух первых беседах демо-ящика: диапазон — больше.
const RANGE: usize = 3;
/// Середина щели между карточками списка и чтения.
const SPLIT_GAP: f32 = 4.;
/// Насколько тянуть разделитель.
const SPLIT_MOVE: f32 = 60.;

fn bounds_known(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).bounds.get().size.width > px(0.)
}

/// Строка — беседа, поэтому выделение считает письма: их больше, чем строк.
fn two_selected(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).selection.len() >= 2
}

fn range_selected(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).selection.len() > RANGE
}

fn menu_open(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.menu.is_some()
}

fn menu_closed(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.menu.is_none()
}

fn none_selected(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).selection.is_empty() && m.menu.is_none()
}

fn list_resized(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.panes.1 < crate::mailer::W_LIST - 30.
}

fn tips(_: &Scene, _: &Mailer, cx: &App) -> bool {
    crate::keytips::shown(cx)
}

fn no_tips(_: &Scene, _: &Mailer, cx: &App) -> bool {
    !crate::keytips::shown(cx)
}

fn palette_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() > 0)
}

fn palette_matched(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.palette.as_ref().is_some_and(|p| {
        let view = p.view.read(cx);
        view.shown() > 0 && view.input.read(cx).value() == "арх" // i18n-ok: ввод сценария
    })
}

fn palette_closed(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.palette.is_none()
}

fn start(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    Ok(())
}

fn mail_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    rows_loaded(m.list.read(cx))
}

fn letters(m: &Mailer) -> usize {
    m.tabs.iter().filter(|t| matches!(t.kind, TabKind::Compose(_))).count()
}

fn one_letter(_: &Scene, m: &Mailer, _: &App) -> bool {
    letters(m) == 1
}

fn two_letters(_: &Scene, m: &Mailer, _: &App) -> bool {
    letters(m) == 2
}

fn letter_active(_: &Scene, m: &Mailer, _: &App) -> bool {
    matches!(m.tabs.active().kind, TabKind::Compose(_))
}

fn main_active(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.is_active(TabKind::Main)
}

fn only_main(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.tabs.iter().count() == 1 && m.tabs.is_active(TabKind::Main)
}
