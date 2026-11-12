# LIN-04: песочница воркера на Linux целиком

lane: sandbox
status: done
deps: [LIN-01]
class: B
files: [crates/worker/src/sandbox/linux.rs, crates/worker/src/lib.rs, crates/worker/tests/, docs/adr/]
read: [crates/worker/src/sandbox/linux.rs, docs/tasks/MAC-03.md, docs/decisions/2026-11-12-linux.md]
checks: [clippy, nextest, ci-linux]
manual: no

## Цель
Песочница воркера и хоста модулей на Linux — боевая, а не «dev»: не слабее Windows (AppContainer + Job Object) и
macOS (Seatbelt).

## Что входит
- Ревизия `sandbox/linux.rs`: seccomp-bpf (список разрешённых вызовов), Landlock (ФС и сеть, по версии ABI ядра),
  `no_new_privs`, пределы (`RLIMIT_AS`, `RLIMIT_NOFILE`, `RLIMIT_CORE` 0).
- Старые ядра и запреты дистрибутива (Landlock выключен, нет непривилегированных user namespaces): воркер не
  стартует без полной песочницы; текст ошибки называет причину.
- Код завершения воркера для текста ошибки (`exit_code` сейчас `None`).
- Тесты полной песочницы сверх общих; диагностика по образцу `windows_probe`: какой запрет мешает старту.
- ADR «Песочница на Linux»: что открыто и почему, минимальное ядро.

## Приёмка
- `linux.yml` зелёный с `POSTMAN_REQUIRE_FULL_SANDBOX=1`.

## Зависимости
LIN-01.

## Итог (2026-11-12)
- `sandbox/linux.rs`: namespaces обязательны (`isolate` до сторожа, причина — sysctl дистрибутива), Landlock до ABI 6
  с отказом без Landlock, seccomp сужен (`read` 0, `write` 1/2, `tgkill` себе), `RLIMIT_AS` от клиента, добавлены
  `NPROC`/`MEMLOCK`/`MSGQUEUE` = 0; код завершения (159 — seccomp, 153 — `SIGXFSZ`).
- Тесты `tests/linux.rs`, `tests/linux_probe.rs`; диагностика `POSTMAN_SANDBOX_DROP`; ADR 0010.
- Облако (ядро 6.18, Landlock ABI 7): `nextest -p postman-worker` с `POSTMAN_REQUIRE_FULL_SANDBOX=1` — 139/139.
  Путь отказа без user namespaces вживую не проверен (sysctl в контейнере не меняется). `linux.yml` — с LIN-01.

