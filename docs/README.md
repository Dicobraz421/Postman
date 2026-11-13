# Postman — для разработчиков

Стартовая страница репозитория ([README.md](../README.md)) — для пользователей. Здесь — всё техническое: устройство,
сборка, выпуск, замеры. Правила для Claude Code — [CLAUDE.md](../CLAUDE.md).

Клиент почты, календаря и контактов: чистый Rust, интерфейс на gpui (DirectX 11 на Windows, Metal на macOS),
раскладка и клавиши classic Outlook. Windows 10/11 x64 — основа, macOS 13+ arm64 — вторая платформа, Linux x86_64 —
третья (AppImage, deb, rpm). Основной сервер — The Post (JMAP); с Stalwart, IMAP + SMTP + CalDAV/CardDAV и
Exchange (EWS) — совместимость по стандартам, через плагины.

## Куда смотреть

| Документ | О чём |
|---|---|
| [decisions/](decisions/), [specs/S0-decisions.md](specs/S0-decisions.md) | Решения владельца — приоритетнее всего |
| [00-ARCHITECTURE.md](00-ARCHITECTURE.md), [adr/](adr/) | Слои, договор плагинов, реестры; отступления от базиса |
| [DEV.md](DEV.md) | Запуск, снимки экранов, стенд, выпуск, диск |
| [tasks/](tasks/) | Карточки задач (что делается сейчас) |
| [PLAN.md](PLAN.md), [specs/](specs/) | План и спецификации этапов |
| [USER-GUIDE.md](USER-GUIDE.md) | Инструкция пользователя (правда о поведении интерфейса) |
| [DESIGN.md](DESIGN.md), [design/](design/) | Дизайн и макеты |
| [manual-tests/](manual-tests/), [MANUAL-TESTS.md](MANUAL-TESTS.md) | Ручные проверки |
| [IDENTITY.md](IDENTITY.md) | Учётные записи, файлы, связь с системой учёта |
| [SMIME.md](SMIME.md), [EXCHANGE.md](EXCHANGE.md), [STALWART.md](STALWART.md), [THEPOST-COMPAT.md](THEPOST-COMPAT.md) | Протоколы и серверы |
| [STALWART-GAPS.md](STALWART-GAPS.md), [PATCHES.md](PATCHES.md) | Пробелы серверов, патчи зависимостей |
| [SIZE.md](SIZE.md), [MEMORY.md](MEMORY.md) | Размер exe, память и бюджеты |
| [PARITY.md](PARITY.md) | Соответствие classic Outlook (генерируется) |
| [HANDOFF.md](HANDOFF.md), [UI-SPIKE.md](UI-SPIKE.md) | Исходное ТЗ; сравнение egui и gpui (история) |

## Структура

| Крейт | Назначение |
|---|---|
| `crates/ir` | HTML (белый список) / text/plain → IR → Markdown-цитата / исходящий HTML. Без I/O, готов к запуску в песочнице. |
| `crates/jmap`, `crates/imap`, `crates/dav`, `crates/ews` | Клиенты протоколов: JMAP (Mail, Calendars, Principals), IMAP + SMTP, CalDAV/CardDAV, Exchange Web Services. |
| `crates/backend-*` | Плагины путей к серверам (`jmap`, `imap`, `dav`, `ews`) и каталога (`ldap` — Active Directory, сертификаты адресатов) поверх договора из `core`. |
| `crates/cache` | Хранилище на redb (ключ-значение, чистый Rust): папки, строки списка, порядок `(папка, -дата, id)` для окон виртуального скролла, тела, вложения, blob'ы, состояния синхронизации. |
| `crates/search` | Полнотекстовый индекс (tantivy): словоформы (Snowball ru+en), опечатки (Левенштейн 1–2), префикс по мере набора, неверная раскладка, ё=е, свежие выше. 50 тыс. писем: 1–4 мс на запрос. |
| `crates/compose` | Ответ/ответ всем/пересылка из IR (как в Outlook: RE:/FW:, ответ сверху), разбор полей адресов, сборка RFC 5322 multipart/alternative. Bcc — только в конверт. |
| `crates/calendar` | Календарь без UI: JSCalendar → UTC, сборка события с участниками, раскладка пересечений, занятость и общее свободное время, приглашения iMIP. |
| `crates/export` | Выгрузка писем в файлы: EML, mbox, Maildir, MSG, PST. Без I/O. |
| `crates/smime` | S/MIME: разбор CMS и проверка подписи (RSA, ECDSA, ГОСТ Р 34.10-2012), цепочка к якорям. Без I/O, только в воркере. |
| `crates/worker` | Процесс `postman-worker`: чужие данные (HTML, вложения, приглашения) → IR в песочнице. Windows: AppContainer, Job Object, mitigation policies (win32k off, только Microsoft-DLL, без динамического кода и дочерних процессов); macOS: Seatbelt `deny default` (ADR 0009). `postman-viewer` — модули просмотра на WASM. |
| `crates/core` | Слой без UI: модель, договор плагинов, синхронизация (push, предзагрузка, отправка), поисковая индексация, файлы вложений, адресная книга для автодополнения. |
| `crates/demo` | Синтетический демо-ящик (`--demo`, снимки экранов, бенч). |
| `app-gpui` | Интерфейс на gpui (`postman.exe`). |
| `viewers/` | Модули просмотра WASM и их SDK (ADR 0007), вне рабочей области. |
| `tools/pack` | Сборка установщика MSI и манифеста winget. |

Слои и кто от кого зависит — [00-ARCHITECTURE.md](00-ARCHITECTURE.md) и `crates/core/tests/layers.rs`; полосы
параллельной работы — `lanes.toml`.

## Запуск

```sh
cargo build --release --workspace                         # postman.exe + postman-worker.exe (должны лежать рядом)
cargo run --release -p postman-gpui -- --demo 50000     # синтетический ящик, без сервера
POSTMAN_URL=https://mail.example.com POSTMAN_USER=u POSTMAN_PASSWORD=p cargo run --release -p postman-gpui
# или POSTMAN_TOKEN=… (Bearer) вместо USER/PASSWORD
cargo test --workspace
```

Снимки экранов (`--screens`), стенд и проверки — [DEV.md](DEV.md). Кэш: `%LOCALAPPDATA%\Postman\`.

Картинки стартовой страницы — `docs/images/`, это снимки сцен dev-сборки на демо-ящике
(`POSTMAN_SCREENS_ONLY=<сцены> cargo run -p postman-gpui -- --screens <каталог>`), без реальных данных.

## Поставка

Выпуски — [Releases](https://github.com/Dicobraz421/Postman/releases): установщик `postman-<версия>-windows-x64.msi`
(все пользователи, `Program Files`, нужны права администратора; он же — пакет winget `Pets.Postman`), архив
`postman-<версия>-windows-x64.zip` (`postman.exe`, `postman-worker.exe`, `postman-viewer.exe`, `README.txt`,
`install.ps1` — установка для себя без прав администратора), отдельные компоненты; для macOS — образ
`postman-<версия>-macos-arm64.dmg` и архив `.zip` с `Postman.app` (`scripts/macos/package.sh`, MAC-06); для Linux x86_64 —
`postman-<версия>-linux-x64.AppImage`, `.deb` и `.rpm` (`scripts/linux/package.sh`, LIN-06); подписи
minisign `.minisig` для самообновления (T3-12) и `SHA256SUMS.txt`. Решение —
[decisions/2026-11-07-installer-msi-winget.md](decisions/2026-11-07-installer-msi-winget.md); как выпустить —
[DEV.md](DEV.md) «Выпуск». Текст страницы выпуска — `.github/release-notes.md`. Пробная сборка — ручной запуск CI или PR, что правит
выпуск (Actions → запуск → Artifacts). Локально — `scripts/windows/package.ps1` после `cargo build --release --workspace`
(в `dist/`; MSI собирает `tools/pack`). Инструкция пользователя — [USER-GUIDE.md](USER-GUIDE.md) «Установка» и
`scripts/windows/README.txt`. Версия exe — из `version` в `Cargo.toml`; значок — `app-gpui/res/postman.ico`
(`scripts/windows/make-icon.ps1`), манифест — `app-gpui/res/postman.manifest`.

### Подпись (решение владельца 1)

Шаги «Подпись exe» и «Подпись MSI» в CI (`scripts/windows/sign.ps1`) подписывают `postman.exe`, `postman-worker.exe`,
`postman-viewer.exe` и MSI (`signtool`, SHA-256, метка времени), если в репозитории заданы секреты (Settings → Secrets
and variables → Actions):

- `WINDOWS_CERT_PFX_BASE64` — сертификат подписи кода с закрытым ключом (.pfx) в base64:
  `[Convert]::ToBase64String([IO.File]::ReadAllBytes("cert.pfx")) | Set-Content cert.b64`, затем
  `gh secret set WINDOWS_CERT_PFX_BASE64 < cert.b64` (и удалить `cert.b64`);
- `WINDOWS_CERT_PASSWORD` — пароль .pfx: `gh secret set WINDOWS_CERT_PASSWORD`;
- по желанию переменная `WINDOWS_TIMESTAMP_URL` — сервер меток времени (по умолчанию `http://timestamp.digicert.com`;
  для внутреннего CA — его сервер, если есть).

Без секретов шаг пропускается с уведомлением, архив — неподписанный (SmartScreen предупреждает при запуске из
распакованного архива; `install.ps1` снимает отметку «из Интернета»). Сертификат на токене (EV, HSM) так не
подключить — нужен отдельный подписывающий агент; решаем, когда будет сертификат.

## Замеры

`postman --demo --bench` — прогрев, 400 кадров листания с открытием писем, 120 кадров ресайза;
печатает одну строку: `exe_mb`, `first_frame_ms`, `scroll_p50/p99/max`, `resize_p50/p99/max` (мс на кадр),
`scroll_cpu_pct` и `scroll_ui_thread_cpu_pct`, то же для листания списка без открытия писем (`list_*`), `ui_cpu_p50/p99`,
`rss_peak_mb`, `private_peak_mb`, затем простой: `idle_cpu_pct`, `idle_rss_mb`, `idle_private_mb` (private bytes).
`POSTMAN_BENCH_SCROLL_FRAMES=N`, `POSTMAN_BENCH_LIST_FRAMES=N` — длиннее фазы, например для профилировщика. Бенч
работает на свежей копии ящика (`bench.redb` из шаблона `bench-template-<n>.redb`). Это общий сценарий для сравнения
машин и версий; бюджеты — [SIZE.md](SIZE.md), [MEMORY.md](MEMORY.md).
