# LIN-01: собирается и тестируется на Linux в CI

lane: ci
status: todo
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

## Что не вошло
- Значки, снимки, пути — LIN-02. Системное — LIN-03. Выпуск — LIN-06.

## Зависимости
Нет.
