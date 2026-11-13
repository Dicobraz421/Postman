# CMF-01: беседа — заглушить / вернуть звук

lane: x/messenger-comforts   # core, app-gpui
status: done
deps: []
class: A
files: [crates/core/src/mute.rs, crates/core/src/newmail.rs, app-gpui/src/comfort.rs, app-gpui/src/newmail.rs, app-gpui/src/live.rs, app-gpui/src/lifecycle.rs, app-gpui/src/list.rs, app-gpui/src/menu.rs, app-gpui/src/features.rs, app-gpui/i18n/*/comfort.ftl, app-gpui/i18n/*/commands.ftl]
read: [docs/decisions/2026-11-14-messenger-comforts.md]
checks: [nextest, screens:comfort-pinned]
manual: yes                  # docs/manual-tests/CMF-01.md

## Сделано
- `postman_core::mute`: `$muted` у всех писем беседы, переключение, наследование, непрочитанные без заглушённых.
- `newmail::scan`: заглушённые — без уведомления и звука; новое письмо беседы — в `Found::inherit`, интерфейс ставит
  признак и на сервере.
- Непрочитанные папок (дерево, трей, Dock, строка меню) — без заглушённых (`reload_folders`).
- Команда `mail.mute` (палитра, меню письма), значок «без звука» в строке, тост с «Отменить».

## Не сделано
- Наследование — только для писем во «Входящих» (их проверяет `newmail`); письма беседы в других папках признак не
  получают, пока беседу не заглушат снова.

## Запрос в дизайн
- Значок «без звука» в строке письма (сейчас «RingerSilent» / Lucide `bell-off` цветом подписей, 13 px, после флага).
- Пункт меню письма и подсказка у значка в строке — нужен ли текст «Беседа без звука» при наведении.

## Проверки
- Тесты: `mute_covers_the_thread_and_new_letters_inherit`, `muted_letters_leave_the_unread_count`,
  `muted_thread_does_not_notify_and_the_new_letter_inherits`.
- Снимок: `comfort-pinned` (значок у первой строки беседы).
