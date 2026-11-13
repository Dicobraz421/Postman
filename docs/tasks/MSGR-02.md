# MSGR-02: быстрый ответ внизу ленты беседы и @упоминания

lane: x/quick-reply         # app-gpui, formats (crates/compose), core (тесты признака)
status: done
deps: [MSGR-01]
class: A
files: [app-gpui/src/quick.rs, app-gpui/src/mentions.rs, app-gpui/src/fragments.rs, app-gpui/src/reading.rs, app-gpui/src/composer.rs, app-gpui/src/actions.rs, app-gpui/src/mailer.rs, app-gpui/src/main.rs, app-gpui/src/parts.rs, app-gpui/i18n/*/quick.ftl, crates/compose/src/mention.rs, crates/compose/src/lib.rs, crates/core/src/filters.rs, crates/core/src/newmail.rs]
read: [docs/decisions/2026-11-14-messenger-comforts.md, docs/tasks/MSGR-01.md, docs/DESIGN.md]
checks: [nextest, screens:reply-fragments, screens:compose-mention, screens:compose-mention-pick, screens:quick-mention, screens:quick-send]
manual: yes                 # docs/manual-tests/MSGR-02.md

> Решение владельца 2026-11-14 (`docs/decisions/2026-11-14-messenger-comforts.md`): быстрый ответ под лентой,
> @упоминания в любом черновике.

## Сделано
- Под лентой беседы вместо строки «Ответить — Имя» — поле «Ответить всем» (`quick.rs`): Enter и Ctrl+Enter (⌘Enter)
  — отправить, Shift+Enter — новая строка; «Развернуть» и «Отправить всем» — кнопками. Отправка — ответ всем на
  открытое письмо через окно письма (`Composer::dispatch`): подпись, «Перед отправкой», окно «Отменить».
- Фрагменты из ленты без открытого ответа — цитатами над полем (× убирает), набранное — ответ на последний.
  Открытый ответ на беседу забирает фрагменты и набранное себе, без отправки.
- @упоминания (`mentions.rs`, `postman_compose::mention`): в тексте письма и в быстром ответе `@` — подсказка
  участников беседы, адресатов письма и адресной книги (↑ ↓, Enter и Tab — выбрать, Esc — закрыть); выбранный —
  `**@Имя**` и в «Кому», если его нет ни в «Кому», ни в «Копии», ни в «Скрытой».
- Признак «упомянули меня» уже был в ядре (`filters::Me::mentioned` по началу текста): тесты на упоминание из окна
  письма — фильтр «Упоминания» и «важные» уведомления (`newmail::important`).
- Окно письма: после переписи поля из модели (подпись, реплики) — оформление Markdown заново (раньше оставались
  отрезки прежнего текста).

## Не сделано
- Упоминание — только в начале текста ловится признаком «упомянули меня» (`preview`); упоминание глубже в длинном
  письме — нет (тело в список не читается; на сервере «Все папки» — по всему телу).
- В полях ответов на реплики (MSGR-01) подсказки упоминаний нет — в своём тексте письма и в быстром ответе.
- Быстрый ответ — только под лентой; в обычном виде письма — прежняя строка «Ответить — Имя».

## Запрос в дизайн
Макетов нет, сделано по DESIGN и токенам темы — выверить:
1. Быстрый ответ под лентой: рамка `stroke` (в фокусе — акцент), скругление `RadiusControl`, поле без своей рамки,
   справа «Развернуть» (тихая 30 px) и «Отправить всем» (главная 30 px, значок «Отправить»).
2. Цитаты фрагментов над полем: полоса слева `StrokeStrong`, подпись «Имя, дата» 12 px, текст до 60 px, × справа.
3. Подсказка упоминаний: карточка `LayerOverlay` с тенью, заголовок «Упомянуть» 12 px, строки «Имя · адрес»
   (адрес — `text2`), выбранная — `picked`. В окне письма — под полем текста, в быстром ответе — над полем.
4. Упоминание в тексте — `**@Имя**`: в поле — полужирно с приглушёнными `**`, в письме — полужирным.

## Проверки
- Тесты: `postman_compose::mention::tests` (запрос у курсора, совпадение, вставка), `mentions::tests` (вставка и
  «Кому» без повторов, себя не подсказываем), `quick::tests` (Enter — отправить, текст и упомянутые — в черновик,
  с фрагментами — ответ на последний), `filters::tests::mention_from_the_composer_is_caught`,
  `newmail::tests::fresh_letters_since_the_mark`.
- Снимки: `reply-fragments` (фрагменты в быстром ответе → «Развернуть»), `compose-mention`, `compose-mention-pick`,
  `quick-mention`, `quick-send`.
- Ручная — `docs/manual-tests/MSGR-02.md`.
