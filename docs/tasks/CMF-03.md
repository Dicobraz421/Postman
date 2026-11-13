# CMF-03: отмена отправки — задержка и обратный отсчёт

lane: x/messenger-comforts   # core, backends, app-gpui
status: done
deps: [S5-06]
class: A
files: [crates/core/src/send_later.rs, crates/core/src/prefs.rs, crates/backend-imap/src/lib.rs, crates/backend-ews/src/sync.rs, app-gpui/src/send_later.rs, app-gpui/src/compose.rs, app-gpui/src/composer.rs, app-gpui/src/settings_sign.rs, app-gpui/i18n/*/send.ftl, app-gpui/i18n/*/settings.ftl]
read: [docs/decisions/2026-11-14-messenger-comforts.md]
checks: [nextest, screens:comfort-undo-send, screens:settings-sign]
manual: yes                  # docs/manual-tests/CMF-03.md

## Сделано
- Путь один, прежний (S5-06): `send_later::undo_release(secs)` — что сказать серверу, `Undo::offer(…, window)` — окно
  на ту же задержку, `Offer::left` — отсчёт. Задержка 0 — `Release::Now`, окна нет.
- `Prefs::replies.undo_secs` (0 / 5 / 10 / 30, по умолчанию 10); «Настройки → Подписи и шаблоны → Ответы и
  пересылка → Отмена отправки».
- Окно «Письмо отправлено · Отменить · N с» обновляется раз в секунду, пока видно.
- IMAP и Exchange отпускают удержанное письмо при выходе через `send_later::release_now` (тест в ядре).

## Для других полос
- Свой путь отправки берёт задержку так: `Model::undo_secs` (окно письма ставит из `prefs.replies.undo()`), команда —
  `send_later::undo_release(secs)`, окно — `SentDraft::undo_secs`.

## Запрос в дизайн
- Окно «Отменить» с отсчётом (сейчас «Отменить · 7 с» в кнопке карточки над строкой состояния).
- Сегменты «Сразу / 5 с / 10 с / 30 с» в «Ответы и пересылка».

## Проверки
- Тесты: `undo_delay_drives_the_release_and_the_window`, `held_letter_leaves_on_time_or_on_exit`,
  `undo_delay_defaults_to_ten_seconds`, `send_holds_for_undo_and_send_later_goes_at_the_time`.
- Снимки: `comfort-undo-send`, `settings-sign`.
