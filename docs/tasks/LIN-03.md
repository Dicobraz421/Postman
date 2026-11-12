# LIN-03: системное на Linux — пароли, mailto, журнал, тема

lane: x
status: done
deps: [LIN-02]
class: B
files: [app-gpui/src/sys/, app-gpui/src/main.rs, app-gpui/src/security.rs, app-gpui/src/settings_security.rs, app-gpui/Cargo.toml, Cargo.lock, crates/worker/src/lib.rs, scripts/linux/, .github/workflows/linux.yml]
read: [docs/decisions/2026-11-12-linux.md, docs/tasks/MAC-02.md, docs/tasks/MAC-04.md, docs/adr/0011-platform-contract.md]
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
- Политика `SandboxNamespaces` на Linux (общий механизм `security.rs`/`sys::shell::policy`, файл в /etc):
  `required` → `Client::namespaces(Namespaces::Required)` для воркера и хоста модулей (ADR 0010, LIN-04b).
- Память в `--bench` (`idle_private_ws_mb` из `/proc/self/smaps_rollup`), все сцены `--screens`.

## Приёмка
- `linux.yml` зелёный; ручная проверка `docs/manual-tests/LIN-03.md` — когда появится стенд (не блокирует).
- После слияния — пересмотр статуса tier 2 владельцем.

## Что не вошло
- Kerberos, S/MIME из системных хранилищ — LIN-05.

## Зависимости
LIN-02.

## Договор платформенных функций (ADR 0011)

| функция | отказ (закрыто/открыто, что видит пользователь) | журнал (уровень, событие) | пределы и права | на диске | политики (ключ, тип) | Fluent | отличие ОС и причина |
|---|---|---|---|---|---|---|---|
| Пароли и токены | закрыто: службы Secret Service нет — ошибка, вход каждый раз строкой; на диск не пишем | ошибки — код ОС, без секрета | сеанс D-Bus зашифрован (`oo7`) | ничего | `StorePasswords`, bool | те же, что на Windows/macOS | — |
| `mailto:` по умолчанию | закрыто: нет `postman.desktop` — ошибка (как вне `.app` на macOS) | ERROR вызова (как на других ОС) | только `mimeapps.list` пользователя | `~/.config/mimeapps.list` (xdg-utils) | — | — | окна «Приложения по умолчанию» нет: выбор сразу, как на macOS |
| Автозапуск | закрыто: ошибка записи — ошибка в «Общих» | ERROR вызова | — | `~/.config/autostart/postman.desktop` | — | — | — |
| Журнал в файл | открыто: stderr — терминал или journald — в файл не пишем | WARN `log: stderr not redirected` | дозапись | `$XDG_STATE_HOME/Postman/postman-<дата>.log`, неделя | — | — | — |
| «Открыть», «Показать в папке» | открыто: нет FileManager1 — открыть каталог (`xdg-open`) | DEBUG запасной путь | по действию пользователя | — | — | — | выделение файла — если файловый менеджер реализует FileManager1 |
| Уведомления | закрыто: нет службы — ошибка вызова (как на Windows) | как на других ОС | — | — | — | — | щелчок по уведомлению окно не возвращает: трея на Linux нет, окно не прячется |
| Тема и акцент | открыто: нет portal — светлая, акцент Fluent | INFO `appearance: settings portal not available` | — | — | — | — | — |
| HTML из буфера | открыто: нет `text/html` или `wl-paste`/`xclip` — вставка текстом | WARN `clipboard: HTML not read` | предел `max` | — | — | — | — |
| Перетаскивание вложения наружу | открыто: не начинается, «Сохранить как» | — | — | — | — | — | отличие: в gpui на Linux нет источника перетаскивания |
| Политики администратора | нераспознанное — WARN, безопаснее; файл не root или правится другими — WARN, не читается | WARN `policy: …` | файл только root, не `o+w`/`g+w` | `/etc/postman/policies.json` (JSON: строка, целое, дробное, логическое → строка) | все ключи Windows и macOS | — | источник по ОС (ADR 0011 п. 5) |
| `SandboxNamespaces` | закрыто: `required` — без namespaces воркер не работает, письмо текстом; непонятное — `required` | WARN значение, INFO `sandbox namespaces are required` | для всех воркеров и хоста модулей процесса | — | `SandboxNamespaces`, строка `optional`/`required` | — | отличие: namespaces — механизм только Linux (ADR 0010) |
| Шифрование диска | открыто: нет блочного устройства или `lsblk` — состояние неизвестно | DEBUG/WARN | — | — | — | те же | отличие: включить LUKS после установки нельзя — кнопки «Шифрование диска» нет |
| Вход без пароля (Kerberos) | открыто: нет билета — обычный вход | — | — | — | — | — | `klist` MIT (на macOS — Heimdal) |
| Память в `--bench` | открыто: нет `smaps_rollup` — поля нет | — | `idle_private_ws_mb` — частные страницы и swap, ближайшее к частному рабочему набору | — | — | — | — |

## Итог (2026-11-12)
- Всё из «Что входит», кроме перетаскивания вложения из окна наружу (нет в gpui на Linux; в окно — штатный drop gpui).
- Буквы на любой раскладке и Ctrl — как на Windows (`keymap` без изменений: на Linux действует ветка не-macOS);
  проверка — ручная, шаг 8.
- `postman.desktop` — один источник, `scripts/linux/data/postman.desktop` (LIN-06, #627); окно — `app_id` `postman`.
- Журнал воркера — каналом (SBX-01, уже в main); здесь — журнал приложения.

