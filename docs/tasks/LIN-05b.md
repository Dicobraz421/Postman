# LIN-05b: корпоративное на Linux — настройки и вход без пароля в интерфейсе

lane: app-gpui
status: todo
deps: [LIN-05, GKIT-02]
class: B
files: [app-gpui/src/]
read: [docs/tasks/LIN-05.md, docs/decisions/2026-11-12-linux.md]
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

## Приёмка
- `linux.yml` зелёный, снимки сцен «Безопасность»; ручная проверка `docs/manual-tests/LIN-05.md` — когда появится стенд.

## Зависимости
LIN-05, GKIT-02 (переход на gpui-kit 0.9.1 правит `app-gpui`).
