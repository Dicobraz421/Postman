# ADR 0010. Песочница на Linux — namespaces + Landlock + seccomp-bpf в самом воркере, без ослабленного режима

Статус: **принято** (владелец, 2026-11-12, вариант «Б» — namespaces по режиму; LIN-04, LIN-04b). Основание — решение `docs/decisions/2026-11-12-linux.md`:
песочница воркера на Linux — seccomp + Landlock, не слабее Windows (AppContainer + Job Object) и macOS (ADR 0009).

## Решение
Воркер и хост модулей (`postman-worker`, `postman-viewer`) запирают себя сами (`sandbox/linux.rs`), до первого запроса.
Rlimits, Landlock, `no_new_privs` и seccomp обязательны: без любого процесс отвечает «песочница: <причина>» и выходит
с кодом 3, письмо показывается текстом. Namespaces — по режиму (аргумент `--namespaces=…` от клиента, `Namespaces`):
`optional` (по умолчанию) — дистрибутив запретил, процесс работает без них, в журнале WARN с причиной (какой sysctl);
`required` — отказ, как без остальных механизмов. `required` включает администратор политикой `SandboxNamespaces`
(читает приложение, LIN-03); неизвестное значение аргумента — отказ. Ослабленного режима (как `relaxed` на Windows)
для остальных механизмов нет.

1. **Namespaces** — `unshare(CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWIPC)` первым делом, пока процесс однопоточный (до
   сторожа времени): своя пустая сеть (только `lo` без адресов) и свой System V IPC. WARN и отказ называют запрет
   дистрибутива: `kernel.apparmor_restrict_unprivileged_userns = 1` (Ubuntu 23.10+), `kernel.unprivileged_userns_clone
   = 0` (старые Debian), `user.max_user_namespaces = 0`.
2. **Пределы**: `RLIMIT_AS` — от клиента (`--memory=…`: воркер 1 ГиБ, хост модулей 512 МиБ; отказ в памяти — аварийное
   завершение, `SIGABRT`), `RLIMIT_CORE = 0`, `RLIMIT_FSIZE = 0`, `RLIMIT_NOFILE = 16`, `RLIMIT_NPROC = 0`,
   `RLIMIT_MEMLOCK = 0`, `RLIMIT_MSGQUEUE = 0`. Время — сторож, код 5.
3. **Landlock** — запрещено всё, что знает ядро до ABI 6, правил-исключений нет: ФС целиком (V1–V5), TCP bind/connect
   (V4), абстрактные unix-сокеты и сигналы вне процесса (V6). Ядро без Landlock или с выключенным (`lsm=` без
   `landlock`) — отказ. Младший ABI принимается: ФС закрыта уже V1, сеть — namespaces и seccomp.
4. **`no_new_privs`**.
5. **seccomp-bpf**, список разрешённых вызовов, остальное — `SECCOMP_RET_KILL_PROCESS` (код 159, «запрещённый
   системный вызов, seccomp»): `read` только из stdin, `write`/`writev` только в stdout и stderr, `mmap`/`mprotect` без
   `PROT_EXEC`, `tgkill` только своему процессу (`abort` при панике); память (`brk`, `munmap`, `mremap`, `madvise`),
   `futex`, сигналы своего процесса (`rt_sigreturn`, `rt_sigprocmask`, `sigaltstack`), `clock_gettime`, `getrandom`,
   `sched_yield`, `getpid`, `gettid`, `close`, `exit`, `exit_group`. Нет `open*`, `socket`, `clone`/`fork`, `exec*`,
   `ioctl`, `ptrace`.

Код выхода (`exit_code`) — как на macOS: код или 128 + номер сигнала, ждём до 500 мс.

## Что открыто и почему
- stdin/stdout — обмен с клиентом; stderr — журнал воркера (по-английски, без содержимого писем).
- Потоки не создаются после запирания: сторож запускается до него.
- `RLIMIT_FSIZE = 0` бьёт по записи в обычный файл: пока журнал на Linux не пишется в файл, stderr — канал или
  терминал. Когда журнал станет файлом (LIN-03), stderr воркера — канал к приложению или снять предел (как на macOS,
  ADR 0009) — решение владельца.

## Минимальное ядро
5.13 с включённым Landlock (`CONFIG_SECURITY_LANDLOCK`, `landlock` в `lsm=`) и разрешёнными непривилегированными user
namespaces. Сеть Landlock — с 6.7, scopes — с 6.12; на младших ядрах их заменяют namespaces и seccomp.

## Проверки и диагностика
- `tests/sandbox.rs`, `smime.rs`, `viewer.rs` с `POSTMAN_REQUIRE_FULL_SANDBOX=1`; `tests/linux.rs` — seccomp убивает с
  кодом 159 на файле, сокете и запуске процесса, пределы памяти воркера и хоста; `tests/linux_probe.rs` — слои держат
  без seccomp (Landlock, namespaces, rlimits).
- Диагностика: `cargo test -p postman-worker --test linux_probe -- --ignored --nocapture` — ядро, ABI Landlock,
  sysctl user namespaces и запуск со снятым по одному механизмом (`POSTMAN_SANDBOX_DROP`, только отладочная сборка).
- `tests/linux_probe.rs`: оба режима namespaces (`POSTMAN_SANDBOX_DROP=userns-deny` имитирует запрет дистрибутива) и
  отказ на неизвестном режиме.
- CI: на раннерах Ubuntu 24.04 `kernel.apparmor_restrict_unprivileged_userns = 1` — в режиме `optional` воркер работает
  без namespaces (WARN); чтобы проверять их, `linux.yml` (LIN-01) может снять запрет `sysctl`.

## Почему namespaces не обязательны по умолчанию (вариант «Б»)
Ubuntu 23.10+ запрещает их непривилегированным процессам из коробки: с обязательными namespaces Postman не открывал бы
письма на самом частом дистрибутиве. Сеть и IPC без namespaces закрывают seccomp (нет `socket`, `connect`, `shm*`,
`msg*`) и Landlock (TCP, абстрактные сокеты); namespaces — третий слой. Молча слой не пропадает: WARN в журнале,
диагностика `linux_probe`, а администратор может потребовать его политикой.

## Отвергнуто
- Namespaces обязательны всегда (вариант «А», LIN-04): воркер не стартовал бы на Ubuntu по умолчанию.
- Namespaces «по возможности» без следа (как было до LIN-04): слой пропадал незаметно.
- bubblewrap/внешний помощник: лишний бинарник и setuid на старых системах; Landlock и seccomp ставит сам процесс.
