# Postman — разработка на стенде

Справочник для агентов и владельца: читать нужный раздел. Правила — `CLAUDE.md`, процесс — скилл `pets-dev`,
устройство — `docs/00-ARCHITECTURE.md`.

## Порядок работы

- Сначала исследуем, потом планируем, потом делаем. Новое — концепт в чате, файлы — после согласования.
- Вопросы владельцу — пакетом «Решения для владельца» (№, вопрос, рекомендация).
- Ручная проверка задачи — файлом `docs/manual-tests/<задача>.md` (не правкой `docs/MANUAL-TESTS.md`).
- Локально — `cargo check`/`clippy` во время работы и один прогон тестов затронутых крейтов перед PR; весь набор
  гоняет CI (`.github/workflows/ci.yml`, только Windows). Архив `Postman-<версия>-windows-x64.zip` собирает job
  `release` на main и бете (`scripts/windows/package.ps1`; подпись — если задан секрет).
- Спека сессии — `docs/specs/Sx.md`; соответствие сессий фазам плана — в `docs/specs/S0-decisions.md`.

## Запуск

```
cargo run --release -p postman-gpui -- --demo 50000     # демо-ящик; exe — target\release\postman.exe
... --bench                                             # замер: строка key=value в stdout
POSTMAN_URL=… POSTMAN_USER=… POSTMAN_PASSWORD=…         # реальный сервер (или POSTMAN_TOKEN)
cargo run --release -p postman-gpui                     # без окружения: сохранённый вход или строка входа; --sign-out — забыть
```

Release-сборка — без консоли (`windows_subsystem`): `--bench`, `--screens`, `--help` пишут в консоль родителя
или в перенаправление (`> файл`, канал). Git Bash и `.cmd` ждут выхода сами; в интерактивном cmd — `start /wait`,
в PowerShell — `| Out-Default`. Ресурсы exe (значок, версия, манифест) — `app-gpui/build.rs`, нужен rc.exe из Windows SDK.
Архив и установка — `scripts/windows/` (`package.ps1`, `install.ps1`, `README.txt`).

Windows: тестам песочницы нужен один раз на каталог сборки (из `cargo metadata`, учитывает `CARGO_TARGET_DIR`):
`icacls (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q`.

## Снимки экранов

```
cargo run -p postman-gpui -- --screens <каталог>
```

Агенты проверяют интерфейс снимками, а не нажатиями в окне: демо-ящик (свежая копия на 2000 писем, удаляется
в конце; `--keep` — оставить), PNG через PrintWindow. Список сцен — `SCENES` в `app-gpui/src/screens.rs`
(комментарий у каждой сцены); сюда сцены не переписываются. `POSTMAN_SCREENS_ONLY=bulk,offline` — только эти
сцены (отладка; агенты снимают только затронутые); на каждый файл строка `screen <имя> <путь> <ш>x<в>` в stdout,
выход 0; ошибка (в том числе снимок одного цвета) — stderr и код 1. Только Windows. Снимки — dev-сборкой
(картинка та же), release — только для замеров.
`--screens <каталог> --live` — живой ящик из `POSTMAN_URL`… (стенд), сцены — `LIVE_SCENES` там же;
без учётной записи в окружении — `firstrun` (первый запуск).

## Диск

H-01: каталог сборки и скрипты — без зашитого `target/`; тесты — только `tempfile` с префиксом `postman-`;
данные бенча — `%LOCALAPPDATA%\Pets\testdata\postman` (рабочая копия удаляется, `--keep` — оставить); чистка —
`pets/scripts/stand/clean.ps1 -Repos . -Testdata` (без `-Apply` только показывает).

## Стенд Stalwart

Сервер — стенд Stalwart (`docs/STALWART.md`): новое поведение сервера сначала тестом в
`crates/backend-jmap/tests/stand.rs`, находки — туда же в документ; чего сервер не умеет или делает неверно — ещё
и строкой в `docs/STALWART-GAPS.md` (список для отчётов и настройки). Непроверенное на стенде — пометка [проверить].

Stalwart принимает отчёты только через портал support.stalw.art (не GitHub); его CONTRIBUTING запрещает код от
ИИ — PR в Stalwart от нас не делаем.

Запуск: `cargo test -p postman-backend-jmap --test stand -- --ignored` с `POSTMAN_STAND_URL` (напрямую по
HTTPS — ещё `POSTMAN_INSECURE_TLS=1`, самоподписанный сертификат стенда), `POSTMAN_STAND_USER1..3`,
`POSTMAN_STAND_PASSWORD1..3` в окружении. Туннель и пароли — у владельца; адреса и учётные данные — только в
локальном окружении, не в репозитории и не в выводе.

## Диагностика песочницы

Какое ограничение не даёт воркеру стартовать (только отладочная сборка):
`cargo test -p postman-worker --test windows_probe -- --ignored --nocapture`. В CI — при ручном запуске
(`workflow_dispatch`).
