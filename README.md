# Postman

Корпоративный клиент почта + календарь + контакты под **Stalwart** по **JMAP**.
Чистый Rust, интерфейс на gpui (DirectX 11), раскладка и хоткеи classic Outlook. Только Windows 10/11 x64.
Правила для Claude Code — [CLAUDE.md](CLAUDE.md). Устройство — [docs/00-ARCHITECTURE.md](docs/00-ARCHITECTURE.md), разработка на стенде — [docs/DEV.md](docs/DEV.md). План — [docs/PLAN.md](docs/PLAN.md). Исходное ТЗ и решения — [docs/HANDOFF.md](docs/HANDOFF.md). Учётные записи, файлы и связь с системой учёта — [docs/IDENTITY.md](docs/IDENTITY.md). Сравнение egui и gpui — [docs/UI-SPIKE.md](docs/UI-SPIKE.md).

## Структура

| Крейт | Назначение |
|---|---|
| `crates/ir` | HTML (белый список) / text/plain → IR → Markdown-цитата / исходящий HTML. Без I/O, готов к запуску в песочнице. |
| `crates/jmap` | Тонкий JMAP-клиент: autodiscover `.well-known/jmap`, произвольные вызовы, Mail (ящики, письма, `*/changes`, флаги, перемещение). Свой, чтобы добавить Calendars/Principals. |
| `crates/cache` | Хранилище на redb (ключ-значение, чистый Rust): папки, строки списка, порядок `(папка, -дата, id)` для окон виртуального скролла, тела, вложения, blob'ы, состояния синхронизации. |
| `crates/compose` | Ответ/ответ всем/пересылка из IR (как в Outlook: RE:/FW:, ответ сверху), разбор полей адресов, сборка RFC 5322 multipart/alternative. Bcc — только в конверт. |
| `crates/search` | Полнотекстовый индекс (tantivy): словоформы (Snowball ru+en), опечатки (Левенштейн 1–2), префикс по мере набора, неверная раскладка, ё=е, свежие выше. 50 тыс. писем: 1–4 мс на запрос. |
| `crates/calendar` | Календарь без UI: события JSCalendar → UTC (часовые пояса, длительности ISO 8601), сборка нового события с участниками, раскладка пересечений дня по колонкам, слияние занятости, сетка и поиск общего свободного времени для «Помощника планирования». Приглашения из писем (iMIP): .ics разбирается в песочнице, карточка «Принять / Под вопросом / Отклонить»; ответ — через календарь сервера, если событие там есть по UID, иначе письмом METHOD:REPLY с импортом в свой календарь. |
| `crates/worker` | Процесс `postman-worker`: HTML/текст → IR в песочнице. Linux: seccomp-bpf + Landlock + rlimits (+ namespaces, если разрешены). Windows: AppContainer, Job Object, mitigation policies (win32k off, только Microsoft-DLL, без динамического кода и дочерних процессов). |
| `crates/core` | Слой без UI: синхронизация (push, предзагрузка, отправка), поисковая индексация, извлечение текста, разбор писем через воркер, файлы вложений, адресная книга для автодополнения (частота, свежесть, транслит, раскладка). |
| `app-gpui` | Интерфейс на gpui (`postman.exe`): почта, окно письма, календарь, «Помощник планирования», вход, тосты. |

## Запуск

```sh
cargo build --release --workspace                         # postman.exe + postman-worker.exe (должны лежать рядом)
cargo run --release -p postman-gpui -- --demo 50000     # синтетический ящик, без сервера
POSTMAN_URL=https://mail.example.com POSTMAN_USER=u POSTMAN_PASSWORD=p cargo run --release -p postman-gpui
# или POSTMAN_TOKEN=… (Bearer) вместо USER/PASSWORD
cargo test --workspace
```

## Поставка

Архив `Postman-<версия>-windows-x64.zip` — артефакт CI (Actions → запуск → Artifacts): `postman.exe`,
`postman-worker.exe`, `README.txt`, `install.ps1`. Локально — `scripts/windows/package.ps1` после
`cargo build --release --workspace` (в `dist/`). Установка — `install.ps1` без прав администратора, инструкция
пользователя — `scripts/windows/README.txt`. Версия exe — из `version` в `Cargo.toml`; значок —
`app-gpui/res/postman.ico` (`scripts/windows/make-icon.ps1`), манифест — `app-gpui/res/postman.manifest`.

### Подпись (решение владельца 1)

Шаг «Подпись exe» в CI подписывает `postman.exe` и `postman-worker.exe` (`signtool`, SHA-256, метка времени),
если в репозитории заданы секреты (Settings → Secrets and variables → Actions):

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
`scroll_cpu_pct` и `scroll_ui_thread_cpu_pct`, то же для листания списка без открытия писем (`list_*`), `ui_cpu_p50/p99`, `rss_peak_mb`, `private_peak_mb`, затем простой:
`idle_cpu_pct`, `idle_rss_mb`, `idle_private_mb` (private bytes на Windows). `POSTMAN_BENCH_SCROLL_FRAMES=N`,
`POSTMAN_BENCH_LIST_FRAMES=N` — длиннее фазы, например для профилировщика. Бенч работает на свежей копии ящика
(`bench.redb` из шаблона `bench-template-<n>.redb`). Это общий сценарий для сравнения машин и версий.

Кэш: `%LOCALAPPDATA%\Postman\` (Windows), `$XDG_DATA_HOME/Postman` (Linux).

## Хоткеи

Ctrl+1/2/3 — почта/календарь/контакты · Ctrl+E — поиск · Ctrl+N/R/Shift+R/F — создать/ответить/всем/переслать ·
Ctrl+Q/U — прочитано/не прочитано · в окне письма: Ctrl+Enter или Alt+S — отправить, Ctrl+Shift+P — просмотр, Esc — закрыть · Insert — флаг · Del — в «Удалённые» · ↑↓ / j k, Home/End, PgUp/PgDn — навигация.

## Статус (неделя 1, спайк)

Готово: IR с тестами (вырезание активного содержимого, ссылки только http(s)/mailto + детект подмены и punycode,
внешние картинки не грузятся, вёрсточные таблицы линеаризуются, защита от глубокой вложенности);
JMAP-сессия, ящики, страницы писем, тела, флаги, перемещение, опрос `Email/changes`; SQLite-кэш;
UI-оболочка с виртуальным списком на 50 тыс. писем.

Неделя 2: композер (отдельное окно, Markdown, превью через воркер, ответ/всем/переслать), отправка через
JMAP (blob upload → Email/import → EmailSubmission с переносом в «Отправленные»), черновики; воркер-песочница
(тесты: чтение ФС, сеть, запуск процессов — отказ; зависание — убийство по таймауту и перезапуск).
Защита от «бомб» вложенности: html5ever квадратичен на глубоких `<div>` — при оценочной глубине > 2000 письмо показывается текстом.

Неделя 3: вложения (полоса под шапкой, «Открыть»/«Сохранить как…», опасные типы только сохраняются,
очистка имён от bidi-подмены, Mark-of-the-Web на Windows), картинки `cid:` по кнопке или для доверенного
отправителя (декодирование PNG/JPEG/GIF/WebP в воркере), вложения в композере (кнопка, перетаскивание),
пересылка с вложениями оригинала; поиск (Enter в поле, «Все папки»): сразу локальные совпадения из кэша,
затем серверный `Email/query` с `text`; догрузка писем при прокрутке (`anchor` после самого старого в кэше).

Push через JMAP EventSource (переподключение с нарастающей паузой, сверка после подключения; опрос раз в 2 мин —
страховка). Фоновая предзагрузка тел (свежие первыми, пачками по 25, до 256 КБ на часть) — чтение без сети;
текст тел — в поисковый индекс (text/plain как есть, HTML — через отдельный воркер-песочницу).

Не проверено (нет стенда/Windows): отправка и черновики против Stalwart; Windows-песочница
(только проверка типов под `x86_64-pc-windows-msvc`).

Дальше:
- Проверить на Stalwart: отправку, черновики, удаление Bcc сервером; Windows-песочницу на Windows.
- Автосохранение черновика, вопрос при закрытии окна письма, вставка картинок в текст.
- Проверить на Stalwart: push (EventSource), предзагрузку тел.
- Композер (Markdown → multipart/alternative), вложения.
- Рендер: настоящий полужирный шрифт (сейчас `strong` = контрастный цвет), выравнивание абзацев, группировка списка по датам, треды.
- Вопросы из HANDOFF §4 (capabilities Stalwart, availability/free-busy) — проверить на тестовом стенде.
- Прогнать `--bench` на целевых Windows-машинах (Intel iGPU, DX12) — числа из контейнера (llvmpipe) не показатель.
