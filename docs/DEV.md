# Postman — разработка на стенде

Справочник для агентов и владельца: читать нужный раздел. Правила — `CLAUDE.md`, процесс — скилл `pets-dev`,
устройство — `docs/00-ARCHITECTURE.md`.

## Порядок работы

- Сначала исследуем, потом планируем, потом делаем. Новое — концепт в чате, файлы — после согласования.
- Вопросы владельцу — пакетом «Решения для владельца» (№, вопрос, рекомендация).
- Ручная проверка задачи — файлом `docs/manual-tests/<задача>.md` (не правкой `docs/MANUAL-TESTS.md`).
- Локально — `cargo check`/`clippy` во время работы и один прогон тестов затронутых крейтов перед PR; весь набор
  гоняет CI (`.github/workflows/ci.yml`, только Windows). Архив `postman-<версия>-windows-x64.zip` собирает job
  `release` на main и тегах `v*` (`scripts/windows/package.ps1`; подпись — если задан секрет); выпуск — ниже.
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
в конце; `--keep` — оставить), PNG через PrintWindow. Сцены — в описях частей (`PART.scenes`, модуль `scenes`
части, ADR 0005; комментарий у каждой сцены), порядок — `PARTS` в `app-gpui/src/parts.rs` и `after` у сцены, которая
встаёт за сценой другой части; сюда сцены не переписываются. `POSTMAN_SCREENS_ONLY=bulk,offline` — только эти
сцены (отладка; агенты снимают только затронутые); на каждый файл строка `screen <имя> <путь> <ш>x<в>` в stdout,
выход 0; ошибка (в том числе снимок одного цвета) — stderr и код 1. Только Windows. Снимки — dev-сборкой
(картинка та же), release — только для замеров.
`--screens <каталог> --live` — живой ящик из `POSTMAN_URL`… (стенд), сцены с `.live()`;
без учётной записи в окружении — `firstrun` (первый запуск).

## Выпуск

Пользователь скачивает только со страницы [Releases](https://github.com/Dicobraz421/Postman/releases); артефакты
Actions — для разработки. Схема общая для Pets (скилл pets-dev, `references/releases.md`):

| Файл выпуска | Что это |
|---|---|
| `postman-<версия>-<os>-<arch>.zip` | Комплект целиком: `windows-x64` сейчас; Linux — `linux-x64.tar.gz` |
| `postman-worker.exe`, `postman-viewer.exe` | Компоненты под своими именами — скачать один файл вместо всего («Нет компонента», S8-38) |
| `SHA256SUMS.txt` | Контрольные суммы всех файлов |

- Тег `v<версия>` — ровно `version` из `Cargo.toml` (CI сверяет). `v0.2.0` — стабильный выпуск, `v0.2.0-beta.1` —
  предварительный (канал «Предварительный» в настройках).
- Как выпустить: PR с новой `version` в `Cargo.toml` (класс B) → merge → `git tag v<версия> origin/main` и
  `git push origin v<версия>`. CI собирает, подписывает, проверяет тесты и публикует (job `publish`); описание — что
  скачать (`.github/release-notes.md`) и список PR.
- Ссылки приложения: «Проверить обновления» — `releases/latest` (стабильный) или `releases` (предварительный);
  «Скачать» у «Нет компонента» — `releases/download/v<своя версия>/<файл>`: компонент той же версии.
- Linux: строка в matrix job `release` и `scripts/linux/package.sh` с теми же выходами; `publish` не меняется.

## Диск

H-01: каталог сборки и скрипты — без зашитого `target/`; тесты — только `tempfile` с префиксом `postman-`;
данные бенча — `%LOCALAPPDATA%\Pets\testdata\postman` (рабочая копия удаляется, `--keep` — оставить); чистка —
`pets/scripts/stand/clean.ps1 -Repos . -Testdata` (без `-Apply` только показывает).

## Сценарии с вводом

`Scene::input` (`app-gpui/src/screens.rs`, сценарии — `app-gpui/src/drill.rs`) шлёт клавиши, текст и мышь через
`Window::dispatch_event` внутри процесса: мимо ОС, поэтому в чужое окно они не попадут. SendInput и управление рабочим
столом по-прежнему запрещены. `ready` сцены — ожидаемый итог шага. Сцены с `on_request` запускаются только по именам в
`POSTMAN_SCREENS_ONLY`. Точки для мыши считаются из модели (`MailList::bounds`). Диалоги Windows, Проводник и настройки
ОС так не проверить: они остаются в ручных шагах.
Кнопки и поля считать из модели не нужно: `Input::Press("id")` щёлкает по элементу, отмеченному `screens::spot(id)` (все кнопки,
сегменты и переключатели настроек отмечены в конструкторах), и ждёт, пока он появится в окне. `Input::Keys` — команды, символов
в поле они не вводят; текст — `Input::Text`. Срок (30 с) — на шаг сценария. «После перезапуска»: `POSTMAN_SCREENS_PREFS=1` — снимки
читают и пишут файл настроек, второй запуск с тем же `LOCALAPPDATA` и без `--lang`/`--theme` (`cal_drills`, `app-gpui/src/drill_cal.rs`).

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
