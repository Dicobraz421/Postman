# CI-LIN: Linux-job в `gate`

lane: ci
status: done
deps: []
class: B
files: [.github/workflows/ci.yml, .github/workflows/linux.yml]
read: [docs/decisions/2026-11-13-linux-tier1.md]
checks: [ci, ci-linux]
manual: no

> Решение владельца 2026-11-13 «linux-tier1»; правка `.github/` — с явного разрешения владельца в чате.

## Сделать
- Job `test` из `linux.yml` — в `ci.yml` как `linux` (`needs: changes`; идёт и на пуш в main — там сохраняется кэш
  `linux`); `gate.needs` += `linux`. В `linux.yml` остаётся `bundle`.

## Приёмка
- В PR с правкой Rust `gate` ждёт `linux`; красный `linux` — красный `gate`; docs-PR — `linux` пропущен, `gate` зелёный.

## Сделано
Патч применён в #648 по разрешению владельца (2026-11-13).
