# LIN-04b: namespaces воркера на Linux — по режиму optional | required

lane: sandbox
status: done
deps: [LIN-04]
class: B
files: [crates/worker/src/sandbox/, crates/worker/src/lib.rs, crates/worker/tests/linux_probe.rs, docs/adr/0010-linux-sandbox.md]
read: [docs/adr/0010-linux-sandbox.md]
checks: [clippy, nextest]
manual: no

## Цель
Решение владельца 2026-11-12 по ADR 0010 (вариант «Б»): дистрибутив запрещает непривилегированные user namespaces —
воркер и хост модулей работают без них; Landlock, seccomp, rlimits, `no_new_privs` обязательны. Администратор может
потребовать namespaces.

## Итог (2026-11-12)
- `Namespaces { Optional (по умолчанию), Required }`, `Client::namespaces(…)`, аргумент воркера `--namespaces=…`;
  неизвестное значение — отказ (код 3).
- `optional` без namespaces — WARN в stderr-журнал с причиной (какой sysctl); `required` — «песочница: user namespaces
  недоступны (…), а режим namespaces — required».
- Тесты `tests/linux_probe.rs`: оба режима (`POSTMAN_SANDBOX_DROP=userns-deny` имитирует запрет), неизвестный режим;
  `nextest -p postman-worker` с `POSTMAN_REQUIRE_FULL_SANDBOX=1` — 152/152.
- ADR 0010 — принято. Чтение политики `SandboxNamespaces` — LIN-03.
