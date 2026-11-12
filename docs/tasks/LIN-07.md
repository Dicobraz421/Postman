# LIN-07: ГОСТ в S/MIME на Linux

lane: x
status: deferred
deps: [LIN-05]
class: B
files: [crates/core/src/smime/, crates/worker/src/, app-gpui/src/, docs/adr/]
read: [docs/decisions/2026-11-12-linux.md, docs/SMIME.md, docs/tasks/LIN-05.md]
checks: [clippy, nextest, ci-linux]
manual: docs/manual-tests/LIN-07.md

> Владелец 2026-11-12: ГОСТ на Linux понадобится, но сильно позже. Карточка отложена до его слова.

## Цель
Подпись и расшифровка писем ГОСТ на Linux — как на Windows.

## Что входит
- Подпись и расшифровка ГОСТ личным ключом через СКЗИ для Linux (КриптоПро CSP): контейнеры СКЗИ, вызовы — в
  `sys`-модуле `crates/core/src/smime`, по образцу `skzi.rs` для Windows.
- Проверка подписи ГОСТ без СКЗИ в воркере (S5-12.1д) — проверить на Linux тестами.
- Нет СКЗИ — пункты ГОСТ скрыты в настройках, письмо показывает понятную причину.
- ADR: выбор СКЗИ и способа вызова.

## Приёмка
- `linux.yml` зелёный; ручная проверка `docs/manual-tests/LIN-07.md` на стенде с СКЗИ.

## Зависимости
LIN-05.
