# SBX-01: журнал воркера — каналом во всех ОС

lane: sandbox
status: done
deps: [SBX-02]
class: B
files: [crates/worker/src/sandbox/, crates/worker/src/lib.rs, crates/worker/tests/, docs/adr/0009-macos-sandbox.md, docs/adr/0010-linux-sandbox.md, docs/00-ARCHITECTURE.md]
read: [docs/adr/0009-macos-sandbox.md, docs/adr/0010-linux-sandbox.md]
checks: [clippy, nextest, ci-windows, ci-macos, ci-linux]
manual: no

## Цель
Владелец, 2026-11-12: stderr воркера и хоста модулей — канал на всех ОС. Было: Windows — `hStdError = null` (строки о
пределе памяти и паники терялись), macOS — унаследованный файл журнала без `RLIMIT_FSIZE`, Linux — унаследован при
`RLIMIT_FSIZE = 0`.

## Итог (2026-11-12)
- stderr — канал: Windows — третий анонимный канал, дочернему только конец на запись (в `HANDLE_LIST`); macOS и Linux —
  `Stdio::piped()`. macOS: `RLIMIT_FSIZE = 0` возвращён.
- Клиент: поток `worker-stderr` читает канал до закрытия; строка ≤ `STDERR_LINE` (2048 байт, длиннее — с пометкой),
  ≤ `STDERR_RATE` (100) строк в секунду, первая лишняя — одна строка WARN. Приёмник — `Client::journal`, по умолчанию —
  stderr приложения (его журнал) с префиксом `worker: `.
- Тесты: `sandbox.rs` `worker_stderr_reaches_the_journal` (все ОС), `macos.rs` — stderr до файла журнала.
- `docs/00-ARCHITECTURE.md` — «Договор процесса воркера»; ADR 0009, 0010 обновлены.
- Linux: nextest 153/153; Windows и macOS — clippy кросс-целью чистый, тесты — в CI.

## Решения для владельца
- Строки воркера идут в stderr приложения, а не через `tracing` (у `postman-worker` нет зависимости `tracing`, а
  Cargo.toml — полоса deps): в файле журнала они без метки времени и уровня. Подключить `Client::journal` к `tracing`
  в приложении — после GKIT-02 (app-gpui).
