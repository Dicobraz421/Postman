# LIN-06: выпуск для Linux — AppImage, deb, rpm

lane: ci
status: done
deps: [LIN-03, LIN-04]
class: B
files: [.github/workflows/ci.yml, .github/release-notes.md, scripts/linux/, tools/pack/, app-gpui/src/update.rs, docs/DEV.md]
read: [docs/decisions/2026-11-12-linux.md, docs/DEV.md, docs/tasks/MAC-06.md]
checks: [clippy, nextest, ci-linux]
manual: docs/manual-tests/LIN-06.md

## Цель
На странице Releases — файлы для Linux x86_64; приложение обновляется само.

## Что входит
- Строка `linux-x64` в matrix job `release` (`ci.yml`), `scripts/linux/package.sh` с выходами `name`, `dir`,
  `assets`, `version`; `publish` не меняется.
- Файлы: `postman-<версия>-linux-x64.AppImage`, `.deb`, `.rpm`; `.minisig` и `SHA256SUMS.txt` — как у остальных.
  В пакетах — три бинаря, `postman.desktop`, значки, зависимости (`libxkbcommon`, `libgssapi-krb5`, Vulkan).
- Сборка на старом glibc (раннер или контейнер), чтобы AppImage шёл на поддерживаемых дистрибутивах.
- Самообновление (`pets-update`): AppImage заменяет себя; deb/rpm — только сообщение и ссылка.
- `docs/DEV.md` «Выпуск»: вместо `linux-x64.tar.gz` — три файла; `release-notes.md` — что скачать.
- Flatpak не делаем (решение).

## Приёмка
- Пробный тег: три файла на странице выпуска, подписи сходятся; установка deb и rpm в контейнерах CI.

## Зависимости
LIN-03, LIN-04.
