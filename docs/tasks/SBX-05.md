# SBX-05: дескрипторы клиента не наследуются воркером — тест (аудит PAR-AUDIT №10)

lane: sandbox
status: done
deps: [SBX-02]
class: B
files: [crates/worker/src/sandbox/, crates/worker/src/lib.rs, crates/worker/tests/linux_probe.rs]
read: [docs/tasks/PAR-AUDIT.md]
checks: [clippy, nextest]
manual: no

## Итог (2026-11-12)
- Сама мера — с SBX-02: Linux `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`, macOS `fcntl(FD_CLOEXEC)` для fd ≥ 3 в
  `pre_exec`; Windows — `HANDLE_LIST` (stdin, stdout, stderr).
- Проба `Probe::Fd` (отладочная сборка): открыт ли дескриптор у воркера.
- Тест `linux_probe.rs` `parent_descriptors_are_not_inherited`: файл, открытый в клиенте без `O_CLOEXEC`, у воркера
  закрыт (`EBADF`); без меры тест падает (проверено). На macOS автотеста нет: у тестов там нет `libc` для `dup`.
- №46 (удаление профиля AppContainer при деинсталляции) — не здесь: это `tools/pack`.
