# PAR-04: одно поведение на всех ОС — app-gpui и пути (аудит PAR-AUDIT)

lane: x
status: done
deps: [LIN-03]
class: B
files: [app-gpui/src/sys/, app-gpui/src/main.rs, app-gpui/src/attachments.rs, app-gpui/src/diagnostics.rs, app-gpui/src/settings_general.rs, app-gpui/i18n/, crates/core/src/paths.rs, crates/core/src/files.rs, crates/core/Cargo.toml]
read: [docs/tasks/PAR-AUDIT.md, docs/adr/0011-platform-contract.md]
checks: [clippy, nextest]
manual: no

## Цель
Строки аудита №11, 19, 20, 22, 23, 24, 36, 38, 40, 41, 43, 47, 48 — одно поведение на Windows, macOS и Linux (ADR 0011).

## Договор платформенных функций (ADR 0011)

| № | функция | отказ | журнал | пределы и права | на диске | политики | Fluent | отличие ОС и причина |
|---|---|---|---|---|---|---|---|---|
| 11 | Перетаскивание вложения | закрыто: нет копии с меткой «из Интернета» — не тащим, тост (как macOS) | ERROR `drag copy not created, drag refused` | — | копия в `%TEMP%\Postman\drag` с Zone.Identifier | — | `att-drag-failed` | остаток: виртуальный файл (FILECONTENTS) pets-gpui-windows пишет получатель без метки — нужен режим «только копия» в pets |
| 19 | Права каталога данных, `open/`, журнала | закрыто: каталог данных не создан — выход с ошибкой | WARN журнал не создан | только владелец: Unix `0700` (`files::private_dir`) | `data_root`, `open/`, журнал | — | `fatal-data-dir` | Windows — наследование от `%LOCALAPPDATA%` (уже только владелец) |
| 20 | Запрещённые к открытию типы | закрыто: только сохранить | — | один список на все ОС: + `.desktop`, AppImage, `.run`, deb/rpm/snap/Flatpak, `bash`/`ksh`/`zsh` | — | — | прежние | — |
| 22 | `mailto:` при сбое экземпляра | открыто: ссылки события ОС доходят до окна и без экземпляра | WARN сбоя экземпляра (прежний) | — | — | — | — | — |
| 23 | Шифрование диска | открыто: неизвестно — `None` | WARN | том данных: Windows — том пути, macOS — `df` + `diskutil info` (FileVault тома), Linux — LUKS под ФС пути | — | — | прежние | macOS: старая ФС без строки FileVault — `fdesetup` только для системного тома |
| 24 | «В домене» / UPN | открыто: нет билета — обычный вход | — | без кэша на всех ОС | — | — | — | остаток: «область билета = домен учётки» — отдельной задачей (нужен домен из учётки) |
| 36 | Системные утилиты | закрыто: папку ОС не узнать — вызова нет, ошибка | как у вызова | Windows — `FOLDERID_System`/`FOLDERID_Windows`, Unix — `/usr/bin` (`paths::system_tool`) | — | — | — | macOS — свои абсолютные `/usr/bin`, `/usr/sbin` |
| 38 | Каталог журнала | — | — | — | Linux `$XDG_STATE_HOME/Postman` — закрыто в LIN-02 | — | — | macOS `~/Library/Logs` (Консоль) |
| 40 | Автозапуск | как прежде | — | — | Windows `Run`, macOS `SMAppService`, Linux `~/.config/autostart` | — | — | отличие: свёрнутым в трей — только Windows (трея на macOS/Linux нет), там — обычное окно |
| 41 | Подсказка `mailto:` | — | — | — | — | — | по факту ОС: `settings-mailto-hint-choose` (Windows), `-confirm` (macOS), `-now` (Linux) | ключи по смыслу, не по ОС |
| 43 | Имя записи пароля | открыто: запись прежнего имени читается и удаляется | — | без учёта регистра на всех ОС | хранилище ОС | — | — | — |
| 47 | «Загрузки», «Документы» | открыто: нет папки ОС — `~/Downloads` | — | Windows — `FOLDERID_Downloads`/`Documents`/`Desktop` (OneDrive, перенос); Linux — `user-dirs.dirs`; XDG — только абсолютные | — | — | — | — |
| 48 | `system_viewers` | закрыто: Program Files не узнать — каталога администратора нет | — | Windows — `FOLDERID_ProgramFiles`, не `%ProgramFiles%` | — | — | — | — |

## Проверки
- `nextest -p postman-gpui -p postman-core` — 606 passed; новые тесты: `linux_executables_are_blocked`,
  `private_dir_is_owner_only`, `system_paths_are_absolute`, `data_volume_filevault` (macOS).
- clippy `-D warnings` linux и `x86_64-pc-windows-msvc`; macOS — CI.
