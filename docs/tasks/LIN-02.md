# LIN-02: значки, пути и снимки на Linux

lane: x
status: done
deps: [LIN-01]
class: B
files: [app-gpui/src/chrome.rs, app-gpui/src/icons.rs, app-gpui/src/sys/, app-gpui/src/main.rs, app-gpui/src/mailer.rs, app-gpui/Cargo.toml, Cargo.toml, Cargo.lock, crates/core/src/paths.rs, crates/demo/src/lib.rs, .github/workflows/linux.yml]
read: [docs/decisions/2026-11-12-linux.md, docs/tasks/MAC-01.md, docs/adr/0011-platform-contract.md]
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

## Договор платформенных функций (ADR 0011)

| функция | отказ (закрыто/открыто, что видит пользователь) | журнал (уровень, событие) | пределы и права | на диске | политики (ключ, тип) | Fluent | отличие ОС и причина |
|---|---|---|---|---|---|---|---|
| Значки шрифта ОС | открыто: нет пары SVG — значок шрифтом (тест `every_font_glyph_has_an_svg` не пускает такой код) | — | — | — | — | — | вне Windows — SVG Lucide (шрифта Segoe Fluent Icons нет), как на macOS |
| Каталоги (`paths`) | открыто: нет `XDG_*`/`HOME` — `~/.local/…`, затем временный каталог | — | права каталога данных — `0700` (LIN-04, #19 аудита) | данные `$XDG_DATA_HOME/Postman`, журнал `$XDG_STATE_HOME/Postman`, модули администратора `/usr/share/postman/viewers`; загрузки/документы — `user-dirs.dirs` | — | — | журнал — каталог состояния XDG (как `~/Library/Logs` на macOS; на Windows — `<данные>\logs`) |
| Снимок окна (`--screens`) | закрыто: ошибка — stderr и код 1 (как на Windows/macOS); Wayland без XWayland — читаемый отказ | stderr сцены | своё соединение X11, только своё окно | PNG в каталог `--screens` | — | — | механизм — X11 `GetImage` (на Windows PrintWindow, на macOS ScreenCaptureKit), итог один: RGBA клиентской области |
| Шрифт интерфейса | открыто: `fc-match` не ответил или семейства нет у gpui — шрифт выбирает gpui | WARN `fonts: … UI font is chosen by gpui`, DEBUG семейство | один вызов `fc-match` при старте | — | — | — | на Windows/macOS — системный шрифт gpui; на Linux — `sans-serif` fontconfig (у gpui на Linux своего выбора по среде нет) |
| Рамка и кнопки окна | открыто: без композитора X11 gpui даёт системную рамку, свои «─ □ ✕» остаются | INFO gpui `falling back to server-side window decorations` | — | — | — | — | Linux: зоны `WindowControlArea` gpui не разбирает — свои обработчики (щелчок, перетаскивание, край окна) |

## Итог (2026-11-12)
- Снимки `list`, `letter`, `attachments`, `preview-pdf`, `calendar` под Xvfb (llvmpipe) — без «квадратиков», значки SVG,
  кириллица; артефакт `screens-linux` в `linux.yml`.
- Эмодзи в теме демо-письма («Обновление регламента 📋») на Linux не нарисован (Noto Color Emoji установлен, глиф
  пропадает в gpui/cosmic-text) — вне Postman, в «Решения для владельца» PR.

