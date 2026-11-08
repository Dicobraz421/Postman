# FAST-1: опыт — ядро gpui-fast + наша платформа на базе gpui-fast-windows

lane: deps
status: todo
deps: []                       # FAST-0 (pets) — не обязателен, независим
class: B
branch: x/gpui-fast            # опыт, в main не мержить; PR — черновик
files: [Cargo.toml, Cargo.lock, app-gpui/Cargo.toml, third_party/gpui-fast-windows/]
read: [docs/PATCHES.md]
checks: [release, screens]
manual: no

> Решение владельца 2026-11-08. Схема: ядро — crate `gpui-fast` зависимостью (не копируем, свой форк ядра не
> ведём); платформа — одна наша, но на базе `gpui-fast-windows`. gpui-kit не обновлять (0.9 — отдельно).
> Разбор: у gpui-fast в платформе ~100 строк крючков + модуль `fast/` (1,3 тыс.); у нас — патчи из
> `pets/crates/pets-gpui-windows/README.md` (кадры по запросу, живой ресайз, фон, старт без чёрного кадра,
> шейдеры в exe, ленивые текстуры путей, WARP, быстрый `layout_line`, `gpui_windows::pets`). Пересекаются
> по сути только в `directx_renderer.rs` (ленивые текстуры путей ↔ их тайлы и `frame.rs`).

## Сделать
- `gpui-kit` — фича `gpui-fast`; версию не трогать.
- Снимок `gpui-fast-windows 0.1.7` → `third_party/gpui-fast-windows` (`[patch.crates-io]` по пути), перенести в
  него все помеченные правки `pets-gpui-windows` (метки как в оригинале).
- `app-gpui`: `gpui_windows` → пакет `gpui-fast-windows`; код приложения не менять.
- Проверить, что в exe одно ядро: `cargo tree -i gpui-pre` (у gpui-kit `gpui` = gpui-pre не опционален);
  если линкуются оба — доклад с размером exe, не обходить.
- Справка в PR: что изменилось в `gpui-fast` 0.1.8–0.1.13 (CHANGELOG/коммиты) — важно для Windows и листания.

## Стоп и доклад (не дотягивать)
- Конфликтные правки выходят за `directx_renderer.rs` или требуют правок ядра gpui-fast.
- Код `app-gpui` не собирается без правок API.

## Приёмка
- Release собирается; снимки сцен совпадают с main (допуск как у `screens`); тесты нашей части патча зелёные.
- В PR: список конфликтов и как разрешены, размер exe main / опыт, вывод `cargo tree -i gpui-pre`.
