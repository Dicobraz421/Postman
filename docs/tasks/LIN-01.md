# LIN-01: собирается и тестируется на Linux в CI

lane: ci
status: done
deps: []
class: B
files: [.github/workflows/linux.yml, scripts/linux/, crates/worker/tests/, app-gpui/src/sys/]
read: [docs/decisions/2026-11-12-linux.md, .github/workflows/macos.yml]
checks: [clippy, nextest, ci-linux]
manual: no

> Владелец 2026-11-12: Postman под Linux, tier 2 (`docs/decisions/2026-11-12-linux.md`). Класс B согласован на
> этап, включая CI.

## Цель
Postman собирается на Linux без предупреждений, тесты проходят, CI выдаёт архив сборки. В `gate` не входит.

## Что входит
- Отдельный `.github/workflows/linux.yml` по образцу `macos.yml` (`ci.yml` не трогается): `ubuntu-24.04`, PR и main.
  Пакеты: `libkrb5-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev libfontconfig1-dev`.
- Job `test`: clippy (`-D warnings`), модули просмотра wasm32, nextest с `POSTMAN_REQUIRE_FULL_SANDBOX=1`
  (seccomp + Landlock на раннере; не ставится — диагностика, а не ослабление).
- Job `bundle`: release (`--features postman-worker/viewer`), `scripts/linux/package.sh` → каталог с `postman`,
  `postman-worker`, `postman-viewer` и `tar.gz` артефактом Actions (не выпуск — выпуск в LIN-06).
- Починить то, что вскроют clippy и тесты на Linux (замер 2026-11-12 — только `cargo check`).
- `--bench --demo 2000` под Xvfb завершается (на замере не завершился за 150 с) — найти причину.

## Приёмка
- `linux.yml` зелёный: clippy, тесты, архив; Windows и macOS CI зелёные.

## Итог (2026-11-12)
- `linux.yml` (`test`: clippy, wasm32, nextest с полной песочницей; `bundle`: release и `scripts/linux/package.sh` →
  `postman-<версия>-linux-x64.tar.gz`). Пакет `tzdata-legacy` — для старых имён поясов IANA в тестах `postman-ews`.
- Локально (Ubuntu 24.04): clippy и nextest вскрыли три места вне `files:` карточки — полоса sandbox и ews
  («Решения для владельца» в PR): `expect()` в `crates/worker/src/sandbox/linux.rs` (clippy); там же `exit_code` всегда
  `None` — тест `watchdog_stops_a_long_request` получает «воркер завершился» вместо кода сторожа; jiff без
  `tzdata-legacy` не знает `America/Buenos_Aires` (у пользователей Ubuntu 24.04 — то же). До правки полосы sandbox
  `linux.yml` красный (в `gate` не входит).
- `--bench --demo 2000` под Xvfb не зависает: debug-сборка на llvmpipe завершается за 65–107 с (кадр ~130 мс, S/MIME
  писем демо-ящика в воркере ~1,6 с); 150 с на замере не хватило из-за параллельной сборки на тех же 4 ядрах.

## Что не вошло
- Значки, снимки, пути — LIN-02. Системное — LIN-03. Выпуск — LIN-06.

## Зависимости
Нет.
