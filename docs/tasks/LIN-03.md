# LIN-03: системное на Linux — пароли, mailto, журнал, тема

lane: x
status: todo
deps: [LIN-02]
class: B
files: [app-gpui/src/sys/, app-gpui/src/main.rs, app-gpui/src/keymap.rs, scripts/linux/]
read: [docs/decisions/2026-11-12-linux.md, docs/tasks/MAC-02.md, docs/tasks/MAC-04.md]
checks: [clippy, nextest, ci-linux]
manual: docs/manual-tests/LIN-03.md

## Цель
Заглушки `not(any(windows, macos))` в `app-gpui/src/sys` заменены стандартами freedesktop. Конец этапа «Запускается».

## Что входит
- Пароли и токены — Secret Service (GNOME Keyring, KWallet); службы нет — вход каждый раз строкой, на диск не пишем.
- `mailto:` и значок — `postman.desktop` (`MimeType=x-scheme-handler/mailto`), «сделать клиентом по умолчанию» —
  `xdg-mime`; автозапуск — `~/.config/autostart`.
- Журнал в файл, «Показать в папке» (`org.freedesktop.FileManager1`, иначе `xdg-open` каталога), уведомления
  (`org.freedesktop.Notifications`).
- Тёмная/светлая тема и акцент — portal `org.freedesktop.appearance`.
- HTML из буфера обмена (`text/html`), перетаскивание вложений из окна и в окно.
- Один экземпляр, клавиши как на Windows (Ctrl), буквы — на любой раскладке (xkb).
- Память в `--bench` (`idle_private_ws_mb` из `/proc/self/smaps_rollup`), все сцены `--screens`.

## Приёмка
- `linux.yml` зелёный; ручная проверка `docs/manual-tests/LIN-03.md` — когда появится стенд (не блокирует).
- После слияния — пересмотр статуса tier 2 владельцем.

## Что не вошло
- Kerberos, S/MIME из системных хранилищ — LIN-05.

## Зависимости
LIN-02.
