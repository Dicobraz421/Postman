# Жизненный цикл писем (S5-04): метки сроков, панель выбора, напоминания, настройки.
# Метки строки: с заглавной только первое слово (DESIGN «Строка письма и метки»).
lifecycle-due = Срок
lifecycle-due-by = До { $when }
lifecycle-due-today = Сегодня
lifecycle-tomorrow-short = завтра
lifecycle-overdue-days = Просрочено { $n } д
lifecycle-overdue-hours = Просрочено { $n } ч
lifecycle-waiting = Ждём ответа
lifecycle-waiting-today = Ждём ответа · сегодня
lifecycle-waiting-days = Ждём ответа · { $n } д
lifecycle-no-reply = Нет ответа · { $n } д
lifecycle-remind-at = Напомнит { $when }
lifecycle-expires = Исчезнет
lifecycle-expires-days = Исчезнет через { $n } д
lifecycle-expires-hours = Исчезнет через { $n } ч
lifecycle-archives = В архив
lifecycle-archives-days = В архив через { $n } д
lifecycle-archives-hours = В архив через { $n } ч
# Кнопки в шапке письма.
lifecycle-act-due = Срок
lifecycle-act-remind = Напомнить
lifecycle-act-wait = Ждать ответа
lifecycle-act-expire = Исчезнет
# Панель выбора.
lifecycle-pick-due = Срок
lifecycle-pick-remind = Напомнить
lifecycle-pick-wait = Ждать ответа
lifecycle-pick-expire = Исчезнет
lifecycle-today = Сегодня
lifecycle-tomorrow = Завтра
lifecycle-friday = В пятницу
lifecycle-next-week = На следующей неделе
lifecycle-later-today = Сегодня позже
lifecycle-monday = В понедельник
lifecycle-days = Через { $n } д
lifecycle-in-days = Через { $n } д
lifecycle-pick-date = Выбрать дату…
lifecycle-pick-date-time = Выбрать дату и время…
lifecycle-date-placeholder = ДД.ММ.ГГГГ
lifecycle-date-time-placeholder = ДД.ММ.ГГГГ ЧЧ:ММ
lifecycle-set = Поставить
lifecycle-bad-date = Не разобрать: дата — ДД.ММ.ГГГГ, время — ЧЧ:ММ.
lifecycle-past = Этот срок уже прошёл.
lifecycle-remove-due = Убрать срок
lifecycle-remove-remind = Не напоминать
lifecycle-remove-wait = Не ждать ответа
lifecycle-never = Никогда
lifecycle-read-only = Папка общего ящика только для чтения: сроки не ставятся.
lifecycle-snoozed = Напомню { $when }.
# Напоминания и тосты.
lifecycle-reminder = Напоминание
lifecycle-open = Открыть
lifecycle-dismiss = Скрыть
lifecycle-no-reply-toast = Нет ответа: «{ $subject }» — письмо снова во «Входящих».
lifecycle-replied-toast = Пришёл ответ: «{ $subject }».
lifecycle-expired-toast = Исчезло писем: { $n }.
lifecycle-newsletters-toast = Рассылки в «Архиве»: { $n }.
# Группа наверху списка: просроченный срок и сработавшие напоминания.
lifecycle-group-top = Срочное
# Имя папки «Архив», если у ящика её нет (создаётся сервером с ролью archive).
lifecycle-archive-folder = Архив
# Настройки → «Быстрые фильтры, сроки и жизненный цикл».
lifecycle-settings-expire-to = Исчезнувшие письма
lifecycle-settings-expire-to-hint = Куда уходит письмо, когда наступает «Исчезнет» — свой срок или код подтверждения.
lifecycle-settings-to-archive = В «Архив»
lifecycle-settings-to-trash = В «Удалённые»
lifecycle-settings-remind = Напоминания
lifecycle-settings-remind-hint = «Напомнить…» убирает письмо в «Архив» и возвращает наверх «Входящих» непрочитанным в срок.
lifecycle-settings-remind-off = Выключено: в срок — только напоминание тостом, письмо остаётся, где лежит.
