# Ядро gpui-fast и одна наша платформа на базе gpui-fast-windows

Статус: **Принято владельцем: вариант А (2026-11-08)**. Доведение остановлено: gpui-kit 0.7.3 не собирается с ядром
gpui-fast 0.1.11 (см. «Итоги варианта А»). Замер — FAST-2 (`2026-11-08-gpui-fast.md`).

## Целевая схема

1. **Ядро — `gpui-fast` с crates.io, зависимостью.** Включается фичей `gpui-kit/gpui-fast`, импорты в коде не
   меняются: `gpui_kit::*` указывает на `gpui_fast`. Своего форка ядра нет, правки в ядро — только через upstream.
2. **Платформа Windows — одна, в pets:** `pets/crates/pets-gpui-windows`, пакет `gpui-fast-windows` **той же версии,
   что ядро** (снимок N с crates.io + наши правки с метками «Postman patch» / «Pets patch»). В снимке ядро закреплено
   точно, `gpui = { package = "gpui-fast", version = "=N" }`: у них `^N`, и без `=` ядро уезжает на N+1, а платформа
   остаётся на N. `Postman/third_party/gpui-fast-windows` — только на время опыта.
3. **Один gpui-kit на всё:** версия и фича `gpui-fast` задаются в рабочей области pets
   (`gpui-kit = { version = "=X", features = ["gpui-fast"] }`). pets-ui, pets-shell и приложения берут тот же gpui-kit.
   Если фичу включит одно приложение, Cargo всё равно включит её во всей сборке, поэтому решать её в каждом
   приложении нельзя.
4. **Один rev pets:** одна ревизия pets в `[patch.crates-io]` (`gpui-fast-windows`) и в зависимостях
   `pets-*` приложения. Сейчас у Postman это `b1e3440`, обе строки меняются вместе.
5. **API для приложений** (`gpui_windows::pets`: перетаскивание, значки, снимок окна, уведомления) переезжает в снимок
   gpui-fast-windows и принимает `gpui_fast::Window`. Приложение зависит от
   `gpui_windows = { package = "gpui-fast-windows", version = "=N" }`. Свой `windows`/`windows-sys` у интерфейса
   по-прежнему не заводится.

## Почему ядро N, а не версия из gpui-kit

`gpui-kit` 0.7.3 требует `gpui-fast = "0.1.7"`, то есть `^0.1.7`. По `Cargo.lock` ядро встаёт на **0.1.11**
(и `gpui-fast-platform`, `-windows`, `-macos` и др. — тоже 0.1.11; `gpui-fast-macros` — 0.1.13). Выше не пускает
gpui-pre 0.3.9: у gpui-fast 0.1.12+ вспомогательные снимки закреплены на `gpui-pre-collections =0.3.10/0.3.11`,
а gpui-kit 0.7.3 тянет `=0.3.9` обязательно. Значит, снимок платформы — **gpui-fast-windows 0.1.11**
(решение владельца: версия платформы = версия ядра).

Две детали резолва при переходе: `unicode-properties` пришлось откатить до 0.1.3
(`gpui-fast-web` 0.1.7 требует `=0.1.3`, а в `Cargo.lock` была 0.1.4). Ядро на 0.1.7 закрепить можно, только
закрепив всё семейство, и тогда `cocoa` откатывается 0.26.1 → 0.26.0 (`gpui-fast-macos` 0.1.7: `cocoa = "=0.26.0"`).

## Что показал опыт на 0.1.11 (стоп по карточке)

Перенос правок `pets-gpui-windows` (ревизия `4ac64b9` — с быстрым `layout_line`, без FAST-0) сделан через
`git merge-file`: база — gpui-pre-windows 0.3.9, наше — pets, их — gpui-fast-windows 0.1.11.

| Файл | Конфликтов | С чем столкнулось |
|---|---|---|
| `gpui_windows.rs` | 0 | — |
| `directx_renderer.rs` | 3 | ленивые текстуры путей ↔ их `pub(crate)` поля (ожидалось); путь к шейдеру ↔ их скругление (`rounded_clip`) |
| `build.rs` | 2 | шейдеры в exe (`D3DCompile`) ↔ их сборка шейдера со скруглением (`fast/rounded_clip_shader_build.rs`, 0.1.9 #76) |
| `platform.rs` | 6 | **кадры по запросу** ↔ их `fast::frame_schedule` (0.1.11 #84, «Schedule Windows frames on demand») |
| `window.rs` | 3 | кадры по запросу ↔ `frame_schedule` (2); показ после первого кадра (`DWMWA_CLOAK`) ↔ их `draw_composed` (1, рядом) |
| `events.rs` | 1 | отложенный `WM_PAINT`: наш `frame_request` ↔ их `defer_draw` |

Обе стоп-причины карточки сработали:
- конфликты не только в `directx_renderer.rs`: за 0.1.8–0.1.11 gpui-fast сделал свои кадры по запросу и своё
  скругление клипа, а это наши файлы `platform.rs`/`window.rs`/`events.rs`/`build.rs`;
- `app-gpui` без правки API не соберётся: в 0.1.11 `ScrollWheelEvent` переименован в `ScrollEvent` с новым полем
  `source`, вариант `PlatformInput::ScrollWheel` → `PlatformInput::Scroll`. В приложении одно место:
  `app-gpui/src/screens.rs` (прокрутка в сценах снимков).

Поэтому release, снимки и размер exe опыта не получены. Состояние слияния лежит в рабочем дереве `x/gpui-fast`
(не в коммите).

## Как довести (если владелец выберет вариант А из PR)

- **Кадры по запросу — взять их `frame_schedule`, наш патч убрать** (`vsync.rs` целиком, ветки в `platform.rs`,
  `window.rs`, `events.rs`). Делают то же: поток GPU спит без запросов; плюс у них пробуждение по уведомлению
  об устройстве, без опроса раз в секунду, и хвосты Direct Manipulation. Проверить, что первый кадр нового окна
  приходит сразу (наш `FRAME_DEMAND.notify()` после создания; у них `schedule_frame` после создания) и что CPU
  в простое ≤ 0,5 % (FAST-2).
- **Шейдеры в exe** — наш `build.rs` сначала вызывает их `fast_rounded_clip_shader::stage()`, потом компилирует
  `generated_path(OUT_DIR)` вместо `src/shaders.hlsl`. В `directx_renderer.rs` убрать их путь к шейдеру
  во время выполнения (у нас байткод вшит).
- **Показ после первого кадра** — оставить оба куска, наш в `draw`, их функции композиции после него.
- **Ленивые текстуры путей** — как задумано: поле `path_textures: Option<PathTextures>`, `PathTextures` и поля
  `pub(crate)`. В `fast/frame.rs` `PathTargets::of` возвращает `Option`, а `draw_batches` создаёт
  `PathTextures::new(device, width, height)`, если `!scene.paths.is_empty()`.
- **`app-gpui/src/screens.rs`** — `PlatformInput::Scroll(ScrollEvent { …, ..Default::default() })`.

## Итоги варианта А (2026-11-08)

Платформа доведена: в `third_party/gpui-fast-windows` конфликтов нет, `cargo clippy -p gpui-fast-windows` проходит
(одно предупреждение — их `composition.rs`), тесты нашей части зелёные: 21 из 21 (`force_warp`, `lazy_path`, `image_sprite`,
`background`, `pets_*`). Как разрешены конфликты:

| Файл | Как |
|---|---|
| `platform.rs` | взят их файл 0.1.11 (`frame_schedule`, `schedule_frame` после создания окна — первый кадр сразу) + наш «DirectComposition выкл.» |
| `window.rs` | их `fast_frame_request`; наши `frame_request`, `frame_waker`/`schedule_frame` убраны; в `draw` — наш фон и снятие `DWMWA_CLOAK`, потом их `schedule_frame`; `draw_composed` и прочие функции композиции — их |
| `events.rs` | их `defer_draw`; живой ресайз и `WM_ERASEBKGND` — наши, без конфликтов |
| `vsync.rs` | их файл (наш патч убран целиком) |
| `build.rs` | сначала их `fast_rounded_clip_shader::stage()`, затем наш `D3DCompile` из `generated_path(OUT_DIR)` |
| `directx_renderer.rs` | путь к шейдеру во время выполнения убран (байткод вшит); `path_textures: Option<PathTextures>`, поля `pub(crate)`; сброс при ресайзе |
| `fast/frame.rs` | `PathTargets::of` → `Option`; `draw_batches` создаёт `PathTextures`, если в кадре есть пути |
| `Cargo.toml` снимка | ядро `=0.1.11`, `[workspace]`, наш `windows-sys` и build-зависимость `windows`; для их тестов — копия `rounded_clip_fixtures.rs` и dev-зависимость `serde_json` (в опубликованном крейте их нет) |

**Стоп: ядро 0.1.11 не подходит к gpui-kit 0.7.3.** gpui-kit 0.7.3 написан под gpui-fast 0.1.7 и требует `^0.1.7`.
В 0.1.8–0.1.11 API ядра поменялся несовместимо: `Modifiers::secondary()` → `secondary(cx)`,
`Keystroke::unparse()` → `unparse(os)`, плюс ошибка заимствования в `gpui-component/src/questionnaire/components.rs:947`.
Ошибок не меньше шести: три в `gpui-base`, три в `gpui-component`. Дальше `gpui-kit` и приложение не проверены.
Чинить можно только правкой gpui-kit или ядра, а это стоп-условие. Поэтому release, снимки, размер exe опыта
и idle CPU не получены.

`cargo tree -p postman-gpui -i gpui-pre -e normal --target x86_64-pc-windows-msvc --depth 1`: gpui-pre 0.3.9 остаётся
(gpui-base, gpui-component, gpui-kit, gpui-kit-assets, gpui-pre-platform, gpui-pre-windows), то есть в сборке два ядра.
`app-gpui` берёт `gpui_windows` из `gpui-fast-windows =0.1.11`. В `screens.rs` правка `ScrollEvent` уже сделана.

## Путь 0.7 → 0.9

gpui-kit 0.9.0: `gpui-pre =0.3.11` (обязательно), `gpui-fast 0.1.13` (по фиче).
1. Довести FAST-1 на 0.7.3 / 0.1.11, замерить (FAST-2), решить.
2. Перенести снимок в pets: `crates/pets-gpui-windows` → пакет `gpui-fast-windows` 0.1.11.
   Postman и Урфин переходят на один rev, а `third_party` из Postman удаляется.
3. Полоса deps pets: gpui-kit `=0.9.0`. Снимок 0.1.11 → 0.1.13 переносится трёхсторонним слиянием: база — чистый
   0.1.11, наше — наш снимок, их — 0.1.13. Перед этим прочитать справку PR (что изменилось в 0.1.12–0.1.13).
   Правки API приложений идут от gpui-kit 0.9 и проверяются сборкой.
4. Потом каждая новая версия gpui-fast — так же: снимок N → N+1 слиянием, ядро `=N+1`.

## Что уходит из патча при приёме в upstream

| Наша правка | Состояние в gpui-fast-windows 0.1.13 | Уходит, когда |
|---|---|---|
| Кадры по запросу | **есть** (`frame_schedule`, 0.1.11) | сразу при переходе (вариант А) |
| Живой ресайз (кадр на `WM_SIZE`, `DXGI_SCALING_STRETCH` у swapchain окна) | нет | примут предложение 1 (`docs/upstream/gpui-fast.md`) |
| DirectComposition выключена по умолчанию | нет | не уходит: только у нас (непрозрачный фон) |
| Фон `WM_ERASEBKGND` цветом прошлого кадра | нет | примут предложение 2 |
| Показ окна после первого кадра (`DWMWA_CLOAK`) | нет | примут предложение 3 |
| Ленивые текстуры путей | нет | примут предложение 4 |
| `GPUI_FORCE_WARP` | нет | примут предложение 5 |
| Быстрый `layout_line` | нет | примут предложение 6 |
| Шейдеры в exe (`D3DCompile` в `build.rs`) | нет | не предлагаем; уйдёт, когда сборка будет только на Windows и fxc.exe найдётся всегда |
| `gpui_windows::pets` (API окна для приложений) | — | не уходит: наш API, кандидат в отдельный крейт pets поверх `raw-window-handle` |

Пакетный вопрос: пока gpui-kit тянет gpui-pre обязательно, в сборке два ядра и две платформы Windows. Непатченный
gpui-pre-windows в release ищет fxc.exe (Windows SDK). Наш `[patch]` на gpui-pre-windows держать, пока gpui-pre не станет
необязательным (предложение 7).
