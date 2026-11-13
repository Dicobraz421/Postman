# SBX-03: уровень по первому ответу, предел памяти одной величиной, сирота при смерти клиента (аудит PAR-AUDIT)

lane: sandbox
status: done
deps: [SBX-01]
class: B
files: [crates/worker/src/sandbox/, crates/worker/src/lib.rs, crates/worker/tests/, docs/00-ARCHITECTURE.md, docs/adr/0010-linux-sandbox.md]
read: [docs/tasks/PAR-AUDIT.md]
checks: [clippy, nextest, ci-windows, ci-macos, ci-linux]
manual: no

## Итог (2026-11-12)
- №16: уровень — по первому ответу: процесс ответил «песочница: …» (отказ запирания, выход 3) — `Level::None` и
  причина в `fallback`, а не «полная».
- №17 (решение оркестратора по поручению владельца 2026-11-12): предел — частная выделенная память на всех ОС. Linux —
  `RLIMIT_DATA` (+ `RLIMIT_AS` ×2, не меньше 4 ГиБ), Windows — commit Job Object, macOS — счётчик кучи (ADR 0009).
- №45: исход «превышен предел памяти» одинаков на всех ОС: клиент узнаёт его по строке std
  `memory allocation of … failed` в stderr (SBX-01) или по коду 6 (macOS).
- №44: сторож следит за родителем (`getppid`, каждые 250 мс) — клиент умер, процесс выходит; на Windows — Job Object.
  `PR_SET_PDEATHSIG` снят: он срабатывал по смерти потока, запустившего воркер, а не процесса.
- Тесты: `sandbox.rs` `worker_memory_limit_has_one_name_everywhere` (все ОС), `linux_probe.rs` — уровень при отказе;
  `linux.rs`, `macos.rs` — одно имя исхода. Linux nextest 156/156; Windows и macOS — clippy кросс-целью, тесты в CI.
- Не автотест: выход сироты (проверен вручную на Linux — процесс вышел сразу после смерти клиента).
