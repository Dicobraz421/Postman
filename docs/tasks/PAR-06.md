# PAR-06: трей на Linux и вход без пароля по домену учётки

lane: x
status: done
deps: [PAR-04]
class: B
files: [app-gpui/src/tray.rs, app-gpui/src/sys/, app-gpui/src/chrome.rs, app-gpui/src/screens.rs, app-gpui/src/signin.rs, crates/core/src/paths.rs]
read: [docs/tasks/PAR-AUDIT.md, docs/adr/0011-platform-contract.md, docs/tasks/PAR-04.md]
checks: [clippy, nextest]
manual: docs/manual-tests/PAR-06.md

## Цель
Убрать отличия №40 (автозапуск в трей) и №24 (смысл «вход без пароля») из PAR-04: одно поведение на всех ОС.

## Что сделано
- Linux: значок в трее — StatusNotifierItem + `com.canonical.dbusmenu` (`sys/tray_linux.rs`, zbus), тот же API, что
  `gpui_windows::pets::tray`: значок и значок «есть непрочитанные» (те же `.ico`), подсказка с числом, меню
  «Открыть»/«Выход», щелчок — показать окно. Закрытие окна (и своя «✕») — в трей по настройке «Закрытие окна».
  Спрятать — X11 `UnmapWindow`; Wayland — свернуть.
- Автозапуск на Linux — с `--minimized` (в трей, как на Windows); на macOS трея нет — окно скрывает система.
- Щелчок по уведомлению на Linux — `ActionInvoked` (`default`) → окно и письмо; сигналы трея обрабатываются и без
  значка (на всех ОС).
- №24: «войти без пароля» — только если домен адреса или сервера учётки совпадает с доменом входа: Windows —
  `USERDNSDOMAIN`, macOS и Linux — область действующего билета Kerberos; без кэша (`signin::same_domain`).

## Договор платформенных функций (ADR 0011)

| функция | отказ | журнал | пределы и права | на диске | политики | Fluent | отличие ОС и причина |
|---|---|---|---|---|---|---|---|
| Значок в трее | открыто: нет хоста значков (`StatusNotifierWatcher`) — значка нет, окно закрывается как раньше | WARN `tray icon not created` (как на Windows) | своё имя D-Bus `org.kde.StatusNotifierItem-<pid>-1` | — | — | `tray-open`, `tray-quit`, `tray-tip-unread` | macOS: трея нет, бейдж в Dock (MAC-07) |
| Закрытие окна в трей | открыто: окно не спряталось — закрывается | ERROR `window not hidden` | — | — | — | `tray-hint-*` | Wayland: спрятать нельзя — свернуть |
| Автозапуск | как прежде | — | — | `~/.config/autostart/postman.desktop` с `--minimized` | — | — | macOS: `SMAppService` без аргументов, окно скрывает система |
| Щелчок по уведомлению | открыто: служба без действий — уведомление без щелчка | WARN не показано | поток на уведомление до закрытия | — | — | `tray-open` (подпись действия) | — |
| Вход без пароля | открыто: домен не совпал или билета нет — обычный вход с паролем | — | без кэша | — | — | прежние | источник домена: Windows `USERDNSDOMAIN`, macOS/Linux — область билета |

## Проверки
- nextest `postman-gpui`, `postman-core` — 609 passed (`passwordless_needs_the_same_domain`, `icons_decode`,
  `autostart_minimized`); clippy linux и `x86_64-pc-windows-msvc`; macOS — CI.
