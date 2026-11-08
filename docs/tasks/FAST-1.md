# FAST-1: опыт — gpui-kit с фичей gpui-fast и нашим патчем Windows

lane: deps
status: todo
deps: []
class: B
branch: x/gpui-fast            # опыт, в main не мержить; PR — черновик
files: [Cargo.toml, Cargo.lock, app-gpui/Cargo.toml, third_party/gpui-fast-windows/]
read: [docs/PATCHES.md, docs/decisions.md]
checks: [release, screens]
manual: no

> Решение владельца 2026-11-08: проверить gpui-fast (Longbridge) как способ снять отступление ADR-GPUI по CPU при
> листании. gpui-kit 0.7.3 уже имеет фичу `gpui-fast` (→ gpui-fast 0.1.7, своя платформа `gpui-fast-windows`).
> gpui-kit не обновлять (0.9 — отдельно): меняем одну переменную.

## Сделать
- `gpui-kit` — фича `gpui-fast`. Версию не трогать.
- Снимок `gpui-fast-windows 0.1.7` → `third_party/gpui-fast-windows` (`[patch.crates-io]` по пути). Перенести в него
  три патча из `docs/PATCHES.md` (шейдеры в exe, кадры по запросу, живой ресайз) и модуль `gpui_windows::pets`
  из `pets/crates/pets-gpui-windows`; метка «Postman patch» как в оригинале.
- `app-gpui`: зависимость `gpui_windows` — на `gpui-fast-windows` (переименование пакета), код приложения не менять.
- Проверить, что в exe не попадают обе сборки gpui (gpui-pre и gpui-fast): `cargo tree -i gpui-pre`, размер exe.

## Стоп и доклад (не дотягивать)
- Перенос патчей и `pets`-модуля > ~300 строк diff или требует правок ядра gpui-fast.
- Код `app-gpui` не собирается без правок API.

## Приёмка
- Release собирается; снимки сцен совпадают с main (допуск как у `screens`).
- В PR: diff переноса по патчам (строки), размер exe main / опыт, вывод `cargo tree -i gpui-pre`.
