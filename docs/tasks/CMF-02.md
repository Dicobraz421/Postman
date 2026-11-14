# CMF-02: закрепить / открепить письмо или беседу

lane: x/messenger-comforts   # core, app-gpui
status: done
deps: []
class: A
files: [crates/core/src/pin.rs, crates/core/src/filters.rs, app-gpui/src/comfort.rs, app-gpui/src/list.rs, app-gpui/src/filters.rs, app-gpui/src/menu.rs, app-gpui/src/drill_mail.rs, app-gpui/i18n/*/comfort.ftl]
read: [docs/decisions/2026-11-14-messenger-comforts.md]
checks: [nextest, screens:comfort-pinned]
manual: yes                  # docs/manual-tests/CMF-02.md

## Сделано
- `postman_core::pin`: слово `pm.pin`, переключение, лимит `LIMIT` = 20 бесед в папке.
- `filters::arrange`: `Hit::pinned` — первыми в любой сортировке, группа `Group::Pinned` над «Срочным» и датами,
  не по дате под ними — `Group::Rest` («Остальные»).
- Список: закреплённые дают свой набор (как «Срочное»); значок булавки в строке.
- Команда `mail.pin` (палитра, меню письма); в виде бесед — вся беседа в папке; тост с «Отменить», при лимите — тост.

Хвосты (волна `x/comfort-tails`, 2026-11-14, владелец: «Хвосты одной волной»):
- «Все папки» и найденное поиском: закреплённые — первыми, группой «Закреплённые» над папками (поиск в папке — над
  датами).

## Не сделано
- Лимит 20 бесед — на папку; в «Все папки» закреплённых может быть больше.

## Запрос в дизайн
- Заголовок группы «Закреплённые» и «Остальные» — сейчас как у групп дат (`filters::group_header`).
- Значок булавки в строке («Pin» / Lucide `pin`, цветом подписей) — место и цвет.

## Проверки
- Тесты: `pin_toggles_and_keeps_the_limit`, `pinned_go_first_in_any_sort` (и «Все папки»).
- Снимок: `comfort-pinned`.
