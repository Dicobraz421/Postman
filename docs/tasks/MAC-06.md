# MAC-06: выпуск для macOS

lane: ci
status: in-progress
deps: [MAC-02, MAC-03]
class: B
files: [.github/workflows/ci.yml, .github/workflows/macos.yml, .github/release-notes.md, scripts/macos/, docs/DEV.md]
read: [docs/DEV.md#выпуск, docs/decisions/2026-11-11-macos.md]
checks: [ci]
manual: docs/manual-tests/MAC-06.md

## Цель
Postman для Mac на странице Releases рядом с Windows: подписан, нотаризован, обновляется.

## Что входит
- Строка `macos-arm64` в matrix job `release` в `ci.yml` (в MAC-01 сборка — отдельный `macos.yml`, без выпуска):
  `postman-<версия>-macos-arm64.zip` (и `.dmg`) в выпуске по тегу, `SHA256SUMS.txt`, блок macOS в
  `.github/release-notes.md` (как открыть без подписи — пока подписи нет).
- Подпись Developer ID и нотаризация (`codesign --options runtime`, `notarytool`, `stapler`) — когда владелец заведёт
  аккаунт Apple Developer; до того — ad-hoc и инструкция «Всё равно открыть» (решение 2026-11-11).
- `.dmg` с ярлыком «Программы».
- `pets-update`: режим «zip с .app» — замена бандла целиком.
- Homebrew cask — по желанию владельца.
- Mac-job в CI pets (`macos-26`: clippy `--target aarch64-apple-darwin` и тесты `pets-shell`, `pets-gpui-macos`,
  `pets-ui`) — сейчас mac-код pets проверяет только CI `macOS` Postman, тесты pets для macOS не запускаются (MAC-04).
- `macos.yml` — в обязательную проверку (`gate`), когда MAC-02…MAC-04 закроют расхождения с Windows.

## Приёмка
- Тег `v*` даёт выпуск с файлами macOS; скачанный на M1 zip/dmg ставится и открывается (с подписью — без
  предупреждения Gatekeeper).

## Сделано
- Строка `macos-arm64` в `release` (`ci.yml`): `.zip` и `.dmg`, `SHA256SUMS.txt`, блок macOS в release-notes;
  `pets-update` заменяет `Postman.app` из `.zip`; шаг подписи Developer ID и нотаризации — по секретам, без них ad-hoc.

**Ждёт:** подпись Developer ID и нотаризация — аккаунт Apple Developer владельца (секреты `MACOS_CERT_*`).

## Зависимости
MAC-02, MAC-03; подпись — аккаунт Apple Developer (владелец).
