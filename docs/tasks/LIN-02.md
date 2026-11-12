# LIN-02: значки, пути и снимки на Linux

lane: x
status: todo
deps: [LIN-01]
class: B
files: [app-gpui/src/chrome.rs, app-gpui/src/icons.rs, app-gpui/src/sys/, crates/core/src/paths.rs, crates/demo/src/lib.rs, .github/workflows/linux.yml]
read: [docs/decisions/2026-11-12-linux.md, docs/tasks/MAC-01.md]
checks: [clippy, nextest, ci-linux]
manual: no

## Цель
Окно на Linux выглядит как на Windows, а агенты проверяют его снимками.

## Что входит
- Значки: шрифта Segoe Fluent Icons на Linux нет («квадратики» на замере) — SVG того же значка, как на macOS
  (`chrome.rs`, `icons.rs`): условие «не Windows» вместо «macOS». Кнопки окна — свои (Fluent), не системные.
- Пути `postman_core::paths` по XDG: данные — `$XDG_DATA_HOME/Postman` (`~/.local/share`), журнал —
  `$XDG_STATE_HOME/Postman`, модули администратора — `/usr/share/postman/viewers`; загрузки и документы — `xdg-user-dirs`.
- Снимок окна для `--screens`: `app-gpui/src/sys` — X11 (`GetImage` клиентской области), в CI под Xvfb; Wayland —
  через XWayland или читаемый отказ. `linux.yml`: сцены `list`, `letter`, `attachments`, `preview-pdf`, `calendar`
  артефактом.
- Шрифт интерфейса: на замере `.SystemUIFont` → DejaVu Sans; выбор по fontconfig, кириллица и эмодзи проверены снимком.

## Приёмка
- Снимки CI: без «квадратиков», сцены не одного цвета.

## Что не вошло
- Все сцены `--screens`, тема и акцент системы — LIN-03.

## Зависимости
LIN-01.
