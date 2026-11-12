# SBX-04: журнал воркера — в `tracing` приложения с уровнями

lane: sandbox
status: done
deps: [SBX-01, GKIT-09]
class: B
files: [crates/worker/src/lib.rs, crates/worker/tests/journal.rs, app-gpui/src/main.rs]
read: [pets: crates/pets-shell/LOGGING.md]
checks: [clippy, nextest]
manual: no

## Итог (2026-11-12)
- `postman_worker::set_default_journal(Fn(JournalLevel, &str))` — приёмник строк stderr всех клиентов без своего
  `Client::journal`; ставится один раз. Без него — stderr приложения, как в SBX-01.
- `JournalLevel::of`: паника, отказ в памяти — ERROR; остальное — WARN (воркер пишет в stderr только о необычном).
- Приложение (`main.rs`, сразу после `init_logging`): `tracing::error!/warn!(target: "worker", "worker: …")` — метка
  времени, уровень, файл и системный журнал по `LOGGING.md`.
- Тест `tests/journal.rs`. Linux: nextest postman-worker, clippy postman-worker и postman-gpui — чисто.
