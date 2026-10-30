# Модули просмотра Postman

Модуль просмотра показывает вложение формата, которого нет среди встроенных, без перекомпиляции Postman
(ADR 0007, решение A). Модуль — WebAssembly без импортов: он получает байты файла и отвечает документом
разделами или картинкой. Запускает его хост `postman-viewer.exe` в песочнице воркера (AppContainer, Job Object),
интерпретатор — `wasmi`; у модуля нет ни сети, ни файлов, ни часов — только его вход.

Здесь — своя рабочая область Cargo (вне корневой, цель `wasm32-unknown-unknown`):

- `sdk/` — `postman-viewer-sdk`: типы ответа (`ViewerOutput`, `Section`, блоки IR), общие пределы, макрос
  `viewer!` с экспортами ABI v1; `no_std` + `alloc`;
- `fb2/` — образец: книга FictionBook `.fb2` → главы разделами; описание — `fb2/fb2.json`.

## Свой модуль на Rust

1. Крейт рядом с `fb2/` и в `members` файла `viewers/Cargo.toml`:

   ```toml
   [package]
   name = "postman-viewer-djvu"
   edition.workspace = true
   version.workspace = true

   [lib]
   name = "djvu"                     # имя файла: djvu.wasm
   crate-type = ["cdylib", "rlib"]   # rlib — для тестов на своей машине

   [dependencies]
   postman-viewer-sdk.workspace = true
   ```

2. Функция просмотра и экспорт:

   ```rust
   use postman_viewer_sdk::{Block, Inline, Section, SectionTitle, ViewerOutput};

   postman_viewer_sdk::viewer!(view);

   pub fn view(input: &[u8]) -> ViewerOutput {
       let text = String::from_utf8_lossy(input);
       let blocks = vec![Block::Paragraph { align: Default::default(), content: vec![Inline::text(text)] }];
       ViewerOutput::Sections { sections: vec![Section { title: SectionTitle::None, blocks }], truncated: false }
   }
   ```

3. Сборка (один раз — `rustup target add wasm32-unknown-unknown`):

   ```sh
   cargo build --manifest-path viewers/djvu/Cargo.toml --release --target wasm32-unknown-unknown
   ```

   Модуль — `viewers/target/wasm32-unknown-unknown/release/djvu.wasm` (или в `$CARGO_TARGET_DIR`). Тесты — обычные
   `cargo test` на своей машине: макрос `viewer!` вне `wasm32` ничего не порождает.

Стандартная библиотека на `wasm32-unknown-unknown` есть (распределитель памяти, коллекции); ввода-вывода нет.
Паника — ловушка: хост покажет «не удалось показать». Ошибку формата лучше вернуть `ViewerOutput::Error(текст)`.
Крейты, которым нужен WASI или JavaScript (`wasm-bindgen`, `getrandom` с `js`), не годятся: у модуля не должно быть
ни одного импорта (`wasm-tools print модуль.wasm | grep import` — пусто), иначе хост его не загрузит.

## ABI v1

Ядро модели WASM (без компонентов), только экспорты:

| Экспорт | Тип | Что делает |
|---|---|---|
| `memory` | память | вход и ответ |
| `pm_abi` | `() -> i32` | версия ABI, `1` |
| `pm_alloc` | `(len: i32) -> i32` | место под вход длины `len`; хост пишет туда файл |
| `pm_view` | `(ptr: i32, len: i32) -> i64` | просмотр входа; итог — указатель ответа в старших 32 битах, длина — в младших |

Ответ — `ViewerOutput` в [postcard](https://postcard.jamesmunns.com/): `Sections { sections, truncated }`,
`Image { width, height, rgba }` (RGBA8) или `Error(текст)`. postcard не описывает себя: порядок вариантов и полей —
договор (тест `wire_format_matches_the_host_schema` в `sdk/`), менять — только новой версией ABI. Модуль на
другом языке повторяет эти экспорты и схему из `sdk/src/lib.rs`.

Хост создаёт модуль заново на каждый просмотр: память между запросами не живёт, освобождать вход и ответ
незачем.

## Пределы

Ответ хост проверяет как чужие данные: ссылки — проверкой ссылок IR (негодная — текстом, у похожей на другой адрес —
пометка), картинки из письма (`Inline::Image`) — подписью. Сверх пределов ответ отвергается **целиком** — модуль
обрезает показ сам и ставит `truncated` (константы — в `postman_viewer_sdk`):

| Что | Предел |
|---|---|
| вход (файл) | 48 МиБ |
| память модуля | 256 МиБ |
| топливо | 10⁹ инструкций WASM на запрос |
| время | 10 с на процесс хоста |
| ответ (postcard) | 8 МиБ |
| разделов | 500 |
| блоков во всех разделах (с вложенными) | 10 000 |
| символов текста | 1 000 000 |
| ячеек таблиц | 200 000 |
| вложенность цитат и списков | 16 |
| картинка | до 2048 px по стороне |

Топливо `wasmi` списывает за весь линейный код участка (тело функции без `if`/`loop`, итерация цикла) при входе в
него, а не за исполненные ветви: большой `match`, встроенный в цикл разбора, стоит тысячи единиц на каждый проход.
Обработчики, которые вызываются на каждый элемент, лучше не встраивать (`#[inline(never)]`). Замена негодного UTF-8
(`String::from_utf8_lossy`) в интерпретаторе на порядок дороже строгой проверки `std::str::from_utf8` — сначала
строгая. Ориентир — образец FB2: роман в 900 000 символов — около 0,4·10⁹ топлива.

## Описание `<имя>.json`

```json
{
  "name": "FB2",
  "version": "1.0",
  "abi": 1,
  "extensions": ["fb2"],
  "mime": ["application/x-fictionbook+xml"],
  "signatures": [{ "prefix": "<?xml", "contains": "<FictionBook" }, { "prefix": [80, 75, 3, 4] }],
  "limits": { "memory": 134217728 }
}
```

- `name`, `version` — подпись в просмотре («Просмотр: модуль «FB2» 1.0»), до 64 символов;
- `abi` — `1`;
- `extensions` — без точки, латиница и цифры; `mime` — без параметров (`; charset=…`);
- `signatures` — первые байты файла: `prefix` — с начала, `contains` (необязательно) — где-то в первых 4 КиБ;
  текстом (UTF-8) или массивом байтов;
- `limits` — свои пределы `input`, `memory`, `fuel`, `output`, не больше общих (больше — модуль не загрузится);
  нет — общий.

Нужно хотя бы одно из `extensions`, `mime`, `signatures`. Встроенный просмотр главнее модуля: модуль для `.docx`
или `.xml` не будет выбран.

## Куда положить

`<имя>.json` и `<имя>.wasm` рядом, в один из каталогов:

- `%ProgramFiles%\Postman\viewers` — для всех пользователей (ставит администратор);
- `%LOCALAPPDATA%\Postman\viewers` — для себя; в просмотре такой модуль подписан «сторонний».

Postman читает каталоги при запуске. Плохое описание или модуль пропускается с записью в журнал. Модуль с тем же
именем файла, что и системный, и модуль для того же расширения уступают системному. Выключить модуль —
«Настройки → Просмотр вложений». Модули этого каталога в пакет установки не входят; `fb2.wasm` CI собирает для
тестов хоста.
