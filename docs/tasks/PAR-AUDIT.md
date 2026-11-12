# PAR-AUDIT: расхождения платформ (Windows · macOS · Linux)

lane: common
status: done
class: A
files: [docs/tasks/PAR-AUDIT.md]
read: []
checks: []
manual: no

## Цель
Повод: stderr воркера на трёх ОС — три разных поведения (Windows — в никуда, macOS — в журнал без предела,
Linux — наследуется при `RLIMIT_FSIZE = 0`; чинит SBX-01). Платформы делались разными волнами без общего договора.
Ниже — другие такие расхождения в **уже реализованном** (отсутствие Linux-частей LIN-02/03/05 не перечисляется).
Чтение кода на `main` 5a1b147 и pets на `main` 2026-11-12; строки — на эти ревизии. `pets:` — `crates/pets-shell/src/`,
если не указан другой крейт.

Общий вывод: у пар «Windows/macOS» нет договора о **поведении при отказе**, **пределах** и **форме политик**.
Предложение сквозное — ADR «договор платформенной функции» (класс B, полоса docs): для каждой функции `sys` —
отказ (закрыто/открыто), текст пользователю, журнал (уровень, язык), пределы, что на диске; таблица ниже — чек-лист.

## Высокий риск

| # | функция | Windows | macOS | Linux | нестыковка | риск | предложение |
|---|---|---|---|---|---|---|---|
| 1 | Повтор при сбое строгой изоляции (`Client::call_for`) | любая ошибка первого запроса (таймаут, падение) → перезапуск с `relaxed` — **без AppContainer** — и тот же вход | `relaxed` игнорируется, процесс и изоляция те же | как macOS | письмо, роняющее/вешающее воркер на первом запросе, само понижает изоляцию и разбирается повторно без AppContainer | безопасность | sandbox, B: повтор без понижения для чужих данных; понижение — только по явному отказу запуска (`create_in_container`), не по ответу. `lib.rs:1345-1361`, `sandbox/windows.rs:135-139` |
| 2 | Уровень «частичная» | только Job Object + mitigations: файлы профиля и сеть доступны | режима нет, всегда `deny default` | режима нет, netns + Landlock | только на Windows чужие данные разбирает процесс с правами пользователя на файлы и сеть; видно лишь слово в строке состояния | безопасность | sandbox, B: «частичная» не ниже Unix — restricted token / low IL + запрет сети (WFP) или отказ «только текст», как Linux. `sandbox/windows.rs:140-157,187-191` |
| 3 | Политики администратора: тип значения (`shell::policy`) | только `REG_SZ` (`RRF_RT_REG_SZ`); `REG_DWORD` → `None` молча | строка, bool → `1/0`, целое → строка | нет | `StorePasswords`/`AllowUpdate` = DWORD 0 (обычно для ADMX) на Windows игнорируются — политика открыта | безопасность | pets (shell, A): `get_policy` читает `REG_SZ` и `REG_DWORD`; ADMX в поставке. `pets:sys/registry.rs:75-95`, `app-gpui/src/sys/shell.rs:80-85`, `security.rs:26-38` |
| 4 | Политика `MinTls` | `REG_SZ "1.3"` закрепляет TLS 1.3 | число 1.3 в профиле → `CFNumber::as_i64` → не «1.3»: не закреплено | нет | одна политика закрепляет на одной ОС и молча нет на другой | безопасность | pets (shell, A): дробные числа → строка; Postman — неизвестное значение политики в журнал `warn`. `pets:sys/managed_macos.rs:29-32` |
| 5 | Итог цепочки S/MIME (`verdict`/`confirm`) | флаги по порядку: прочие ошибки цепочки (ограничения имён, неизвестное критическое расширение) вместе с «отзыв неизвестен» → `Unknown`, значок «подписано» | любая ошибка первой оценки → `Rejected` | — | Windows пропускает то, что macOS отклоняет | безопасность | core, B: сначала маска «всё, кроме отзыва» → `Rejected(Failed)`, затем отзыв. `crates/core/src/smime/windows.rs:153-173`, `macos.rs:162-165` |
| 6 | Генератор случайных (ключ содержимого, IV) | код `BCryptGenRandom` не проверяется | `SecRandom` → `/dev/urandom` → при отказе `error!` и **нулевой буфер** дальше | — | на обеих ОС сбой генератора не останавливает шифрование, и по-разному | безопасность | core, B: `random` → `Result`, отказ шифрования. `smime/cng.rs:47-56`, `seal_win.rs:47-50`, `keychain.rs:167-177` |
| 7 | Назначение ключа при проверке подписи | только EKU emailProtection; `CertVerifyCertificateChainPolicy` не вызывается (в шапке обещана BASE) | `kSecPolicyAppleSMIME`: EKU и KeyUsage | — | одна подпись: Windows «Good», macOS `WrongUsage` | безопасность | core, B: KeyUsage (digitalSignature/nonRepudiation) — своей проверкой в общем коде для обеих. `smime/windows.rs:207-218`, `mod.rs:5-7` |

## Средний риск

| # | функция | Windows | macOS | Linux | нестыковка | риск | предложение |
|---|---|---|---|---|---|---|---|
| 8 | `lockdown` при ошибке | всегда `Ok` (`let _ = ok`) | ошибка → `Err`, выход 3 | ошибка → `Err`, выход 3 | Windows открыт при сбое, Unix закрыт | безопасность | sandbox, B: проверить политику `GetProcessMitigationPolicy`, иначе `Err`. `sandbox/windows.rs:439-451` |
| 9 | ОС не даёт изоляцию | политика против AppContainer → «частичная», работает | ослабления нет | AppArmor `restrict_unprivileged_userns` → выход 3, только текст | одна ситуация — ослабление на Windows и отказ на Linux | безопасность / UX | sandbox, B: одно правило для всех (см. #2) + текст в строке состояния. `sandbox/linux.rs:95-120` |
| 10 | Наследование дескрипторов воркером | `HANDLE_LIST`: только stdin/stdout | `Command` без чистки fd; `RLIMIT_NOFILE=16` открытые не закрывает | то же; Landlock открытые fd не ограничивает | комментарий «только stdin/stdout/stderr» верен лишь для Windows | безопасность | sandbox, B: в `pre_exec` закрыть fd ≥ 3 (`close_range`). `sandbox/windows.rs:348`, `macos.rs:61-63`, `linux.rs:39-51` |
| 11 | Перетаскивание вложения: метка «из Интернета» | основной формат — виртуальный файл (FILECONTENTS), файл пишет получатель без Zone.Identifier; сбой копии — только `error!`, перетаскивание идёт | только копия с карантином; сбой — тост, отказ | — | Windows открыт, macOS закрыт | безопасность | app-gpui + pets-gpui-windows, B: без метки — отказ, как macOS. `app-gpui/src/attachments.rs:549-556,609-612` |
| 12 | Обновление: файл между проверкой подписи и установкой | `open_locked` (`FILE_SHARE_READ`) | блокировки нет | как macOS | «проверено = установлено» только на Windows (атакует тот же пользователь) | безопасность | pets (update, A): проверять и распаковывать из одного открытого дескриптора. `pets:pets-update/src/verify.rs:17-27` |
| 13 | Политика обновления: источник | `Software\Policies\Aahz\<продукт>\AllowUpdate` (DWORD) + ключ Postman (`REG_SZ`) | только управляемый `AllowUpdate` | нет | у одной политики разные имена, типы, источники | безопасность / админ | pets (update) + app-gpui, B: один реестр имён политик (`docs/` таблица) и одно чтение через `shell::policy`. `app-gpui/src/update.rs:116-119,205` |
| 14 | Окружение воркера | `USERPROFILE`, `LOCALAPPDATA`, `APPDATA`, `TEMP`… (в «частичной» — реальные каталоги) | `env_clear()` | `env_clear()` | только Windows отдаёт пути профиля (имя пользователя) | безопасность | sandbox, B: в AppContainer — только то, что нужно; в «частичной» — пути контейнера/пустые. `sandbox/windows.rs:194-206` |
| 15 | Дампы при падении воркера | аналога `RLIMIT_CORE=0` нет (WER/LocalDumps может сохранить память с письмом) | `RLIMIT_CORE=0` | `RLIMIT_CORE=0` | память с чужими данными может лечь на диск только на Windows | данные | sandbox, B: `WerAddExcludedApplication`/`SetErrorMode` + `WER_FAULT_REPORTING_NO_UI`. `sandbox/windows.rs:264-269` |
| 16 | Уровень изоляции, который видит клиент | реальный (Full/Partial + причина) | `Full` до `lockdown`; при его отказе — ответ «песочница: …», статус остаётся Full | как macOS | Unix показывает Full при неработающей песочнице | UX / безопасность | sandbox, B: уровень — из первого ответа воркера. `macos.rs:99`, `linux.rs:88`, `lib.rs:746` |
| 17 | Предел памяти: смысл | `ProcessMemoryLimit` (commit процесса) | `Limited` — только куча Rust | `RLIMIT_AS` — адресное пространство | одно число — три величины; письмо проходит на одной ОС и падает на другой | данные / UX | sandbox, B: договорить величину, Linux — запас на стеки/резервы; тест «большое письмо» на трёх ОС. `windows.rs:269`, `macos.rs:161-191`, `linux.rs:150` |
| 18 | Kerberos в журнале LDAP | только код `DsGetDcNameW`, debug | полный текст GSS minor на info и в тексте ошибки (принципал, узлы); EWS на той же ОС — только debug | — | внутренние имена в журнале | безопасность | backends, A: minor — debug, как EWS. `crates/backend-ldap/src/sys/macos.rs:164-186` |
| 19 | Права каталога данных и `open/` (открытые вложения) | наследуются от `%LOCALAPPDATA%` | внутри `~/Library` | `~/.local/share/Postman` по umask, права не задаются | на Linux вложения могут читать другие пользователи | безопасность / данные | core, A: `0700` на `data_root`, `open/`, журнал (часть LIN-02 по сути — сверить). `crates/core/src/paths.rs:27-40`, `files.rs:228-232` |
| 20 | Список запрещённых к открытию типов | исполняемые Windows | `.app/.pkg/.command/.scpt/.terminal` | открытие работает, но нет `.desktop`, `.AppImage`, `.run`, `.deb`, `.rpm`, `.flatpakref` | список не учитывает Linux | безопасность | core, A: дополнить `BLOCKED` — один список на все ОС. `crates/core/src/files.rs:13-151` |
| 21 | Запись пароля | `CredWriteW` — замена за шаг | `delete` → `SecItemAdd`: сбой добавления стирает старый | так же (delete + CreateItem) | на Unix сбой записи теряет пароль | данные | pets (shell, A): `SecItemUpdate`, иначе add. `pets:sys/credentials_macos.rs:31-35`, `credentials_linux.rs:86-92` |
| 22 | `mailto:` при сбое единственного экземпляра | ссылка из argv обрабатывается всегда | ссылки (`url_rx`) передаются окну только внутри `if let Some(listener)`: при ошибке `single_instance` молча пропадают | — | письмо из ссылки не открывается, в журнале пусто | UX / данные | app-gpui, A. `app-gpui/src/main.rs:259-266,369-371` |
| 23 | Шифрование диска | том данных (`bitlocker(path)`) | FileVault загрузочного тома, `path` не учитывается | — | данные на внешнем томе — «зашифрован» | безопасность (ложная уверенность) | app-gpui, A: `diskutil info <том данных>`. `app-gpui/src/sys/shell_macos.rs:210-230` |
| 24 | «В домене» / UPN | `USERDNSDOMAIN` при каждом вызове; `whoami /upn` | любой билет Kerberos (`kinit`), кэш `OnceLock` на запуск | — | разный смысл «вход без пароля»; на macOS смену билета не видно до перезапуска | UX / авторизация | app-gpui, A: без кэша, область билета = домен учётки. `shell_macos.rs:258-280` |
| 25 | «Молча» в хранилищах ключей | флаг на вызов (`*_SILENT_FLAG`) | `SecKeychainSetUserInteractionAllowed(0)` на весь процесс | — | параллельный вызов с окном теряет окно и получает отказ | UX / данные | core, B: сериализовать все вызовы Keychain одним замком. `smime/keychain.rs:84-111` |
| 26 | Промежуточные сертификаты для воркера | хранилища `CA` пользователя и машины | пусто | пусто | письмо без промежуточных: Windows «доверено», macOS «не доверено» | UX | core, A. `smime/windows.rs:92-98`, `macos.rs:99-100` |
| 27 | Корни и запреты (`load_trust`) | `Disallowed` отдельным списком | доверие корня — из TLS-настроек; запрет только для S/MIME не исключает корень | пустые якоря | разная логика выбора корней | безопасность | core, B: брать доверие для политики S/MIME. `smime/macos.rs:60-91` |

## Низкий риск (UX, журнал, диск)

| # | функция | Windows | macOS | Linux | нестыковка | риск | предложение |
|---|---|---|---|---|---|---|---|
| 28 | Неверный PIN/пароль ключа | `Failed` | `NeedPin` | — | «ошибка» против «Ввести» | UX | core, A: коды `NTE_INCORRECT_PASSWORD`/`SCARD_W_WRONG_CHV` → `NeedPin`. `smime/cng.rs:27-33`, `sign_win.rs:26-33` |
| 29 | Тайм-аут проверки отзыва | 10 с на цепочку | нет, две сетевые оценки подряд | — | долгое ожидание на macOS | UX | core, A: общий предел. `smime/windows.rs:17`, `macos.rs:115-124` |
| 30 | Причина отказа ОС (Fluent) | `BadSignature` → `smime-reason-signature` | ветки нет → `smime-reason-failed`; «ещё не действует» → `Expired` | — | разный текст для одного случая | UX | core, A. `smime/macos.rs:135-146` |
| 31 | Цепочка своего сертификата при подписи | флаги 0: AIA-загрузка разрешена | сеть запрещена | — | сетевой запрос при подписи только на Windows | приватность | core, A: `CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL`. `smime/sign_win.rs:154-163` |
| 32 | Свои сертификаты / адресная книга | весь `My` (и без ключа); `AddressBook` без предела | только с ключом; ≤ 1000, без сертификатов без адреса | — | разные списки в «Безопасности» | UX / ресурсы | core, A. `smime/seal_win.rs:145-163`, `keychain.rs:432-454` |
| 33 | Отзыв ГОСТ без СКЗИ | CRL через `CryptRetrieveObjectByUrlW` | `fetch_crl` всегда `Err` → `warn` «CRL unreachable» на каждую подпись | так же, текст «Windows-only» | журнал врёт про «недоступен» | журнал | core, A: `NotCheckedGost` без `warn`. `smime/macos.rs:182-184`, `other.rs:20-22` |
| 34 | Язык и уровень журнала S/MIME | тексты по-русски (`sign_win.rs:79,303,309`); не подошёл ключ — debug | по-английски; — warn | — | нарушено «журнал — по-английски», разные уровни | журнал | core, A |
| 35 | Контроллер домена | `DsGetDcNameW`, debug | DNS SRV по области билета, 5 с, info | `None` | разные источник, тайм-аут, уровень | журнал | backends, A. `backend-ldap/src/sys.rs:11-39` |
| 36 | Системные утилиты | `explorer.exe`, `control.exe`, `whoami.exe` по относительному имени | абсолютные `/usr/bin/...` | — | подмена утилиты из PATH/папки программы | безопасность (низкий) | app-gpui, A: путь от `GetSystemDirectoryW`. `app-gpui/src/sys/shell.rs:132-183` |
| 37 | Системный журнал | INFO+ в журнал «Приложение» | ничего | ничего | `--help` везде обещает «Windows Application log» | UX / поддержка | pets (shell) + app-gpui, A: os_log/journald или текст по ОС. `pets:sys/windows.rs:259-292`, `app-gpui/src/main.rs:189` |
| 38 | Каталог журнала | `<данные>\logs` | `~/Library/Logs/Postman` | `<данные>/logs` (не `XDG_STATE_HOME`, как в LIN-02) | удаление профиля не удаляет журнал на macOS | данные | core, A. `crates/core/src/paths.rs:44-51` |
| 39 | Журнал в файл у основного процесса | только если stderr пуст; `NUL` — файла нет | если stderr закрыт или `/dev/null` | как macOS | при запуске планировщиком без журнала на Windows | поддержка | pets (shell), A. `pets:sys/console.rs:74-79` |
| 40 | Автозапуск | `--minimized`, сразу в трей; статус — только наличие Run | `SMAppService` без аргументов, полное окно; при «нужно разрешение» — «вкл» до перечитывания | — | одна настройка — разное поведение | UX | app-gpui + pets, A. `app-gpui/src/sys/shell.rs:35-37,87-90` |
| 41 | Клиент `mailto:` по умолчанию | открывает «Приложения по умолчанию» | назначает сразу | — | текст «Система попросит подтвердить» верен только для macOS | UX | app-gpui, A: два ключа Fluent (без названия ОС). `i18n/ru/settings.ftl:720` |
| 42 | Единственный экземпляр | `Local\` + номер сеанса, `PIPE_REJECT_REMOTE_CLIENTS`; чужой mutex → «уже запущен» | сокет в `$TMPDIR` (0700); ошибка замка → второе окно | без `XDG_RUNTIME_DIR` — общий `/tmp` по umask | разная реакция на сбой; на Linux имя может занять другой пользователь | безопасность / UX | pets (shell), A. `pets:sys/unix.rs:31-91` |
| 43 | Имя записи пароля | без учёта регистра | с учётом регистра | — | адрес в другом регистре — новая запись на macOS | данные | core/app-gpui, A: нормализовать URL и имя. `app-gpui/src/account.rs:53-54` |
| 44 | Смерть UI | `KILL_ON_JOB_CLOSE` | механизма нет: сирота до EOF/сторожа | `PR_SET_PDEATHSIG` (от потока, не процесса) | разный срок жизни сироты | ресурсы | sandbox, B. `sandbox/linux.rs:51` |
| 45 | Превышение памяти: текст | `0xC0000409` «fail-fast» | «превышен предел памяти» | SIGABRT «аварийное завершение» | понятная причина только на macOS | UX | sandbox, A |
| 46 | След на диске | постоянный профиль AppContainer `Postman.MailWorker`, не удаляется | ничего | ничего | след при деинсталляции | данные | ci (pack/msi), A: удаление в деинсталляторе. `sandbox/windows.rs:229` |
| 47 | Пути «Загрузки/Документы» | `%USERPROFILE%\Downloads` (без `SHGetKnownFolderPath`, OneDrive) | `~/Downloads` | без `XDG_DOWNLOAD_DIR`; относительный `XDG_DATA_HOME` принимается | сохранение в несуществующую папку | UX / данные | core, A (Linux-часть — в LIN-02). `crates/core/src/paths.rs:17-19,54-72` |
| 48 | `system_viewers` | `%ProgramFiles%` из окружения (подменяем) | постоянный `/Library/...` | — | каталог пользователя выдаётся за админский (`third_party=false`) | безопасность | core, A: `SHGetKnownFolderPath(FOLDERID_ProgramFiles)`. `crates/core/src/paths.rs:75-83` |

## Не проверено
- WER/`__fastfail` (#15) и `PDEATHSIG` от потока (#44) — свойства ОС, по коду проекта не подтверждены.
- Слой шифрования Kerberos в `ldap3` на Windows/Linux (`ldap://`): на macOS свой код требует privacy.
