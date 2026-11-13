# DEPS-OO7: одна копия aes/cbc/cipher — oo7 0.7 после перехода gpui на zbus 6

lane: deps
status: todo
deps: []
class: A
files: [Cargo.toml, Cargo.lock]
read: [docs/PATCHES.md]
checks: [clippy, nextest, ci-linux]
manual: no

## Почему сейчас две копии
- В `Cargo.lock` — по две версии `aes` (0.8 и 0.9), `cbc` (0.1 и 0.2), `cipher` (0.4 и 0.5). Старые (aes 0.8, cbc 0.1,
  cipher 0.4) тянет только `oo7 0.6` (Secret Service на Linux) — через `gpui-pre-linux` (форк pets, `[patch]`) и
  `pets-shell` (пароли на Linux, SHELL-LIN-01). Наш код и S/MIME — на новых версиях.
- `oo7 0.7` (на новых aes/cbc/cipher) требует `zbus 6`, а `gpui-pre-linux 0.3.12` — на `zbus 5`: две копии zbus хуже
  двух копий шифров (больше кода, два соединения D-Bus). Проверка: `cargo tree -i cipher@0.4.4 --target all`.

## Когда вернуться
- gpui (gpui-pre-linux) перешёл на zbus 6 — поднять `oo7` до 0.7 в pets (`pets-shell`, форк `pets-gpui-linux`) и
  Postman (`zbus`, `ashpd` в `app-gpui/Cargo.toml`) одним пином pets.

## Готово, когда
- `cargo tree -d` не показывает aes/cbc/cipher; Secret Service на Linux — ручная проверка LIN-03 (пароль сохранён и
  прочитан после перезапуска).
