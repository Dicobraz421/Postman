//! Сценарии с вводом на живом ящике стенда (`--screens --live`): разделы 9, 14, 34 `docs/MANUAL-TESTS.md`. Вторую сторону
//! (письмо коллеги, остановка сервера) играет `docs/stand/peer.py`: он читает строки `screen …` прогона.

use gpui_kit::*;

use crate::Mailer;
use crate::screens::{Held, Input, Scene, rows_loaded};

pub(crate) const PART: crate::part::Part = crate::part::Part { name: "drill-live", scenes: SCENES, ..crate::part::Part::NONE };

/// Тема письма, которое коллега присылает во время прогона (`peer.py`).
const PUSHED: &str = "Postman push";

const SCENES: &[Scene] = &[
    // Раздел 9, шаг 3: папки и письма стенда, синхронизация закончилась.
    Scene::new("live-sync", start, synced).live().on_request(),
    // Раздел 9, шаг 6: открытое письмо прочитано и на сервере (сверяет `peer.py`).
    Scene::new("live-read", start, opened).input(READ).live().on_request(),
    // Раздел 9, шаг 7: письмо пришло без перезапуска, выделение не прыгнуло.
    Scene::new("live-push", start, pushed).input(PUSH).live().on_request(),
    // Разделы 9, шаг 8, и 34, шаг 3: поиск словоформ на сервере.
    Scene::new("live-search-1", start, found).input(SEARCH_1).live().on_request(),
    Scene::new("live-search-2", start, found).input(SEARCH_2).live().on_request(),
    // Разделы 9, шаг 9, и 29, шаг 11: связь пропала — ящик читается из кэша.
    Scene::new("live-offline", start, offline).live().on_request(),
    // Связь вернулась.
    Scene::new("live-online", start, online).live().on_request(),
    // Раздел 14, шаг 3: сервер отклонил пароль — строка входа.
    Scene::new("live-rejected", start, rejected).live().on_request(),
    // Раздел 14, шаг 1: большой ящик догружается страницами.
    Scene::new("live-big", start, far_down).input(BIG).live().on_request(),
];

const READ: &[Input] = &[Input::Until(mail_ready), Input::Click(row0, Held::No)];

const PUSH: &[Input] = &[Input::Until(mail_ready), Input::Click(row1, Held::No), Input::Until(opened)];

const SEARCH_1: &[Input] = &[Input::Until(mail_ready), Input::Keys(&["ctrl-e"]), Input::Text("договоры")];

const SEARCH_2: &[Input] = &[Input::Until(mail_ready), Input::Keys(&["ctrl-e"]), Input::Text("поставкой")];

const BIG: &[Input] = &[Input::Until(mail_ready), Input::Click(row0, Held::No), Input::Keys(&["end"]), Input::Until(mail_ready)];

fn start(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
    Ok(())
}

fn row(m: &Mailer, cx: &App, i: usize) -> Option<Point<Pixels>> {
    let list = m.list.read(cx);
    let b = list.bounds.get();
    (b.size.width > px(0.)).then(|| point(b.origin.x + b.size.width / 2., b.origin.y + px(list.row_h * (i as f32 + 0.5))))
}

fn row0(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, 0)
}

fn row1(m: &Mailer, _: &Window, cx: &App) -> Option<Point<Pixels>> {
    row(m, cx, 1)
}

fn mail_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
    rows_loaded(m.list.read(cx)) && !m.list.read(cx).computing
}

fn synced(s: &Scene, m: &Mailer, cx: &App) -> bool {
    m.live.synced && mail_ready(s, m, cx)
}

fn opened(_: &Scene, m: &Mailer, cx: &App) -> bool {
    crate::screens::reading(m, cx).is_some_and(|r| r.doc.is_some())
}

/// Новое письмо наверху, выбранное до него письмо осталось выбранным: это уже не первая строка.
fn pushed(_: &Scene, m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    let on_top = list.row(0).is_some_and(|r| r.subject.contains(PUSHED));
    let open = crate::screens::reading(m, cx).map(|r| r.row.id.clone());
    let picked = list.selected.and_then(|i| list.row(i)).map(|r| r.id.clone());
    on_top && picked.is_some() && picked == open && list.selected != Some(0)
}

fn found(_: &Scene, m: &Mailer, cx: &App) -> bool {
    m.list.read(cx).search_ids.as_ref().is_some_and(|ids| !ids.is_empty())
}

fn offline(s: &Scene, m: &Mailer, cx: &App) -> bool {
    m.offline.is_down() && mail_ready(s, m, cx)
}

fn online(_: &Scene, m: &Mailer, _: &App) -> bool {
    !m.offline.is_down() && m.live.synced
}

fn rejected(_: &Scene, m: &Mailer, _: &App) -> bool {
    m.live.signin.is_some()
}

/// Выбрана строка дальше первой страницы: ящик дочитан до конца.
fn far_down(s: &Scene, m: &Mailer, cx: &App) -> bool {
    let list = m.list.read(cx);
    mail_ready(s, m, cx) && list.selected.is_some_and(|i| i + 1 == list.count) && list.count > BIG_ROWS
}

/// Строк в большом ящике заведомо больше.
const BIG_ROWS: usize = 500;
