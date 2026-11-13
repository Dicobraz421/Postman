# LIN-05b: корпоративное на Linux — настройки и вход без пароля в интерфейсе

lane: app-gpui
status: review
deps: [LIN-05, GKIT-02]
class: B
files: [app-gpui/src/smime_files.rs, app-gpui/src/settings_security.rs, app-gpui/src/settings_tab.rs, app-gpui/src/main.rs, app-gpui/i18n/, crates/core/src/prefs.rs]
read: [docs/tasks/LIN-05.md, docs/decisions/2026-11-12-linux.md, docs/adr/0011-platform-contract.md]
checks: [clippy, nextest, ci-linux, screens]
manual: docs/manual-tests/LIN-05.md

## Цель
Возможности LIN-05 (нижние слои) доступны из интерфейса на Linux; чего нет — скрыто, а не падает.

## Что входит
- S/MIME, «Настройки» → «Безопасность»: «Добавить сертификат из файла…» (портал выбора файла, `.p12`/`.pfx`), пароль —
  строкой в окне, проверка `postman_core::smime::check_personal_file`; ошибки `PersonalFileError` — строками Fluent
  (неверный пароль, не PKCS#12, не поддержано: EC/ГОСТ). Путь — в настройках, пароль — `pets_shell::credentials`
  (Secret Service), не на диск. При запуске и после изменения — `load_personal_files`, при выходе —
  `clear_personal_files`. Удаление файла из списка — с подтверждением строкой.
- Пункты, которых на Linux нет, — скрыть: смарт-карты и PIN («Ввести»), ГОСТ/СКЗИ (до LIN-07), «Другие пользователи».
- Вход без пароля в Exchange: `sys::shell::upn` и `in_domain` на Linux — билет Kerberos по умолчанию (`klist -s`,
  принципал из кэша), как `shell_macos.rs`; без билета — «нет», вход с паролем.
- Тексты — Fluent, оба языка, без названий ОС.

## Сделано (ветка `x/lin-05b` от `x/gkit-09`)
- `app-gpui/src/smime_files.rs`: пути — `prefs.security.smime_files` (ядро), пароль — `sys::credentials`
  (`Postman:smime:<путь>`); при запуске (`main.rs`, только живой ящик) и после изменения — `load_personal_files`.
  «Безопасность → Свои сертификаты»: «Добавить из файла…» (портал выбора файла), пароль строкой, «Открыть»/«Отмена»;
  состояние файла (открыт · N, нужен пароль, причина); «Ввести пароль»; «Убрать» с подтверждением строкой.
- Скрытие недоступного: в настройках пунктов смарт-карт, PIN, СКЗИ и «Других пользователей» нет; строки «нужен PIN»
  и «нужно СКЗИ» показываются только при таком отказе (ключи из файла PIN не просят; ГОСТ — LIN-07). Подсказка «нет
  своих сертификатов» — свой ключ по смыслу (`settings-certs-none-file-hint`).
- `sys::shell::upn`/`in_domain` и `sys::credentials` на Linux — `app-gpui/src/sys` (полоса LIN-02/03); согласовано
  сообщением, до их волны вход без пароля в Exchange не предлагается, пароль файла спрашивается каждый запуск.

## Договор (ADR 0011)
| функция | отказ | журнал | пределы и права | на диске | политики | Fluent | отличие ОС и причина |
|---|---|---|---|---|---|---|---|
| Свой ключ из файла PKCS#12 | закрыто: файл не открыт — ключи его не используются, причина строкой у файла | WARN `smime: personal certificate file not opened: <причина>` (без пути и пароля) | файл ≤ 1 МБ, KDF ≤ 10⁷ итераций | только путь в настройках | — | `settings-certs-file-*` | отличие: Windows и macOS — хранилище системы (`My`, Связка ключей), на Linux его для ключей нет (решение 2026-11-12) |
| Пароль файла | нет хранилища — пароль до выхода, спрашивается снова (не на диск) | WARN `smime: certificate file password not saved …` | — | нигде | `StorePasswords` — не применяется (пароль файла не пароль ящика) | `settings-certs-file-need-password` | — |

## Приёмка
- `linux.yml` зелёный, снимки сцен «Безопасность»; ручная проверка `docs/manual-tests/LIN-05.md` — когда появится стенд.

## Зависимости
LIN-05, GKIT-02 (переход на gpui-kit 0.9.1 правит `app-gpui`).
