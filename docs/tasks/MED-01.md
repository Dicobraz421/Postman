# MED-01: «Медиа беседы» — ядро

lane: x/thread-media-core      # core, demo
status: done
deps: []
class: A
files: [crates/core/src/thread_media.rs, crates/core/src/samefile.rs, crates/core/src/lib.rs, crates/core/src/render.rs, crates/demo/src/media.rs, crates/demo/src/lib.rs]
read: [docs/decisions/2026-11-13-thread-media.md]
checks: [nextest -p postman-core -E 'test(thread_media) | test(samefile)', nextest -p postman-demo -E 'test(media)']
manual: no                  # docs/manual-tests/MED-01.md — ручных шагов нет, интерфейс — MED-02

> Решение владельца 2026-11-13 (`docs/decisions/2026-11-13-thread-media.md`): «Медиа беседы», как «Общие медиа» в
> Telegram. Интерфейс — MED-02.

## Сделано
- `postman_core::thread_media`: `collect(letters, facts, me) -> ThreadMedia { files, media, links, contacts, meetings,
  loaded, total }`, `filter(&ThreadMedia, query)`, `missing`/`fetch` (докачка тел — `Command::Body`, как при
  открытии письма; письма «На этом компьютере» не качаются), `gather`/`load` — чтение хранилища и воркер (IR тел,
  хэши байтов из кэша блобов, кадры маленьких картинок, приглашения, визитки, мои ответы), вызывать вне потока
  интерфейса.
- `postman_core::samefile`: `FileKey`, `same_file`, `Groups` — одинаковые вложения по SHA-256 байтов (ключ частей
  ADR 0006), иначе блоб, иначе имя и размер; разные известные хэши не сводятся.
- Правило подписей (`signatures`) и пикселей (`is_tiny`, `MIN_SIDE` = 32), пометки ссылок (`link_kind`: отписка,
  счётчик переходов).
- Демо (`postman_demo::media`): беседа `media` в «Архиве» месяц назад — файл, пересланный дважды, подпись у двух
  авторов, пиксель 1×1, фото, схема в тексте, видео, ссылки (в том числе только в цитате, `mailto:`, `javascript:`),
  отписка и счётчик, vCard, приглашение. Тела и байты — по запросу пути демо (`postman_demo::body`, `blob`), как у
  остальных писем демо: в хранилище сначала только строки и получатели.
- Тесты: сводка файлов, подписи и пиксели, ссылки без цитат и схем, участники, встречи, поиск, частично загруженная
  беседа, сбор из хранилища без воркера.

## Не сделано
- Хэш частей писем «На этом компьютере» из таблицы `part` напрямую: нет связи «вложение → часть» в API хранилища;
  сейчас хэш считается по байтам из кэша блобов (тот же SHA-256), иначе — блоб, имя и размер.
- Интерфейс — MED-02.
