# Календарь (S3-05, вид — S5-11): неделя, подробности события, открытие занятости.
cal-today = Сегодня
cal-today-tip = Перейти к сегодня
cal-new-meeting = Создать встречу
cal-new-meeting-tip = Создать встречу

# Виды и поиск (S5-21).
cal-view-day = День
cal-view-day-full = День
cal-view-work-week = Раб. неделя
cal-view-work-week-full = Рабочая неделя
cal-view-week = Неделя
cal-view-week-full = Неделя
cal-view-month = Месяц
cal-view-month-full = Месяц
cal-month-more = +{ $n } ещё
cal-short-day = { $wd }, { $day } { $month }
cal-search-placeholder = Поиск в календаре
cal-search-found = Найдено: { $n }
cal-search-nothing = Ничего не найдено. Ищем по теме, месту, описанию и участникам.
cal-search-server = Ищу на сервере…
cal-search-failed = Сервер не ответил ({ $error }) — показано найденное в загруженных днях
cal-search-scope-server = На сервере — год назад и вперёд; Enter — к событию
cal-search-scope-loaded = По всем событиям календаря; Enter — к событию
cal-legend-accepted = Приняли: { $n }
cal-legend-tentative = Под вопросом: { $n }
cal-legend-declined = Отклонили: { $n }
cal-legend-none = Нет ответа: { $n }
cal-loading = Загрузка…
cal-day = { $d ->
    [0] Пн
    [1] Вт
    [2] Ср
    [3] Чт
    [4] Пт
    [5] Сб
   *[6] Вс
}
cal-weekday = { $d ->
    [0] Понедельник
    [1] Вторник
    [2] Среда
    [3] Четверг
    [4] Пятница
    [5] Суббота
   *[6] Воскресенье
}
cal-month = { $m ->
    [1] Январь
    [2] Февраль
    [3] Март
    [4] Апрель
    [5] Май
    [6] Июнь
    [7] Июль
    [8] Август
    [9] Сентябрь
    [10] Октябрь
    [11] Ноябрь
   *[12] Декабрь
}
cal-month-of = { $m ->
    [1] января
    [2] февраля
    [3] марта
    [4] апреля
    [5] мая
    [6] июня
    [7] июля
    [8] августа
    [9] сентября
    [10] октября
    [11] ноября
   *[12] декабря
}
cal-month-title = { $month } { $year }
cal-long-day = { $wd }, { $day } { $month }
cal-minutes = { $n } мин
cal-hours = { $n } ч
cal-busy = занят
cal-tentative = под вопросом
cal-unavailable = вне офиса
cal-failed = Календарь: { $error }

# Слева: мои календари, категории, занятость.
cal-my-calendar = Мой календарь
cal-group-mine = Мои календари
cal-group-categories = Категории
cal-group-share = Доступ к календарю
cal-share-open = Открыть календарь коллеге
cal-share-label = Открыть мой календарь коллеге
cal-share-placeholder = адрес коллеги
cal-share-button = Открыть
cal-share-cancel = Отмена
cal-share-note = Коллега видит ваш календарь, только если вы его открыли. Пока коллега не открыл свой, в помощнике у него «нет данных», а не «свободен».
cal-share-bad = Не адрес: { $email }
cal-share-sending = Открываю календарь: { $email }…
cal-share-done = Календарь открыт: { $email }

# Неделя.
cal-all-day-1 = Весь
cal-all-day-2 = день
cal-all-day = весь день
cal-waits-answer = ждёт ответа

# Подробности события.
cal-details-empty = Выберите встречу — подробности появятся здесь.
cal-details-recurring = Повторяющееся событие
cal-join = Подключиться
cal-join-tip = Открыть ссылку видеовстречи в браузере
cal-chip-organizer = Вы организатор
cal-chip-waits = Ждёт ответа
cal-chip-tentative = Под вопросом
cal-chip-declined = Отклонено
cal-chip-accepted = Принято
cal-invited-by = { $name } приглашает вас
cal-accept = Принять
cal-tentative-answer = Под вопросом
cal-decline = Отклонить
cal-answered-accepted = Вы приняли — ответ отправлен организатору
cal-answered-tentative = Вы ответили «Под вопросом» — ответ отправлен
cal-answered-declined = Вы отклонили — событие скрыто из сетки
cal-change-answer = Изменить ответ
cal-conflict = Пересекается с «{ $title }», { $time }
cal-conflict-more = и ещё { $n }
cal-participants-n = Участники · { $n }
cal-you = { $name } (вы)
cal-organizer = организатор
cal-optional = необязательный
cal-external = Внешний
cal-write-participants = Написать участникам
cal-details-reminder = Напоминание за { $time }
cal-details-reminder-start = Напоминание в начале встречи
cal-details-organizer-time = У организатора: { $time } · { $zone }

# Вкладка встречи.
meeting-title-new = Новая встреча
meeting-tab-titled = Новая встреча · { $title }
meeting-send = Отправить
meeting-send-tip = Отправить приглашения
meeting-all-day = Весь день
meeting-private = Частная
meeting-private-tip = Частная: участники видят только «Занят»
meeting-repeat = Повтор
meeting-repeat-none = Не повторяется
meeting-subject = Тема
meeting-subject-placeholder = Тема встречи
meeting-required = Обязательные
meeting-optional = Необязательные
meeting-rooms = Переговорная
meeting-location = Место / ссылка
meeting-location-placeholder = Переговорная или ссылка
meeting-when = Начало – конец
meeting-date-placeholder = дд.мм.гггг
meeting-start-placeholder = чч:мм
meeting-duration = Длительность
meeting-description = Описание
meeting-desc-add = Добавить описание
meeting-desc-hide = Свернуть описание
meeting-desc-hint = повестка, материалы, ссылки
meeting-description-placeholder = Повестка встречи
meeting-attendees-placeholder = Адрес или имя — Enter или «;»
meeting-external-short = внешний
meeting-presence-tip = { $name } — { $state } в выбранное время
meeting-presence-free = свободен
meeting-presence-unknown = занятость неизвестна
meeting-presence-external = внешний, занятость неизвестна
meeting-me = Вы
meeting-role-organizer = Организатор
meeting-role-required = Обязательный
meeting-role-optional = Необязательный
meeting-role-room = Переговорная
meeting-not-counted = { $role } · не учтён
meeting-only-free = Только общее свободное время
meeting-with-optional = Учитывать необязательных
meeting-legend-free = Все свободны
meeting-legend-busy = Занято
meeting-legend-unknown = Неизвестно
meeting-legend-external = Внешний
meeting-not-counted-n = Не учтены: { $n } — занятость неизвестна
meeting-slots-hint = { $prev } { $next } — по свободным окнам, { $pick } — выбрать
meeting-participants-n = Участники · { $n }
meeting-everyone = Все участники
meeting-known = { $known } из { $n } с данными о занятости
meeting-add-person = Добавить участника
meeting-row-hidden = Календарь вам не открыт — «нет данных», а не «свободен»
meeting-row-external = Внешний адрес — приглашение уйдёт письмом
meeting-external-note = Внешним участникам приглашение уйдёт письмом
meeting-busy-loading = Запрашиваю занятость…
meeting-busy-failed = Занятость не получена: { $error }
meeting-sending = Отправляю приглашения…
meeting-bad-time = Дата или время не распознаны: { $text }
meeting-bad-addresses = Не адреса: { $list }
meeting-no-server = Нужен сервер: в демо-режиме приглашения не отправляются.
meeting-failed = Не отправлено: { $error }
meeting-sent = Приглашения отправлены: { $title }
meeting-created = Встреча создана: { $title }
meeting-untitled = (без темы)

# Приглашение в письме (карточка, S5-11).
invite-day = { $wd }, { $day } { $month }
invite-month-short = { $m ->
    [1] янв
    [2] фев
    [3] мар
    [4] апр
    [5] мая
    [6] июн
    [7] июл
    [8] авг
    [9] сен
    [10] окт
    [11] ноя
   *[12] дек
}
invite-organizer = Организатор: { $name }
invite-accepted-of = { $n } из { $total } приняли
invite-updated = Организатор изменил: { $what }
invite-changed-time = время
invite-changed-place = место
invite-changed-title = тему
invite-done-accepted = Вы приняли
invite-more = Как ответить
invite-more-all = Другие ответы
invite-with-comment = { $s ->
    [accepted] Принять с комментарием…
    [tentative] Под вопросом с комментарием…
   *[declined] Отклонить с комментарием…
}
invite-no-reply = { $s ->
    [accepted] Принять без ответа организатору
    [tentative] Под вопросом без ответа организатору
   *[declined] Отклонить без ответа организатору
}
invite-comment-title = { $s ->
    [accepted] Принять — комментарий организатору
    [tentative] Под вопросом — комментарий организатору
   *[declined] Отклонить — комментарий организатору
}
invite-comment-placeholder = Например: опоздаю на 10 минут
invite-comment-send = Отправить
invite-comment-cancel = Отмена
invite-sample-comment = В это время у меня суд, предлагаю перенести на четверг
invite-done-tentative = Вы ответили «Под вопросом»
invite-done-declined = Вы отклонили
invite-route = Ответ уйдёт через календарь сервера · из invite.ics, разобран в песочнице
invite-route-done = Ответ записан в календарь сервера по UID события · из invite.ics, разобран в песочнице
invite-cancelled = Встреча отменена организатором · { $organizer }
invite-remove = Удалить из календаря
invite-removing = Удаляю…
invite-remove-hint = Убрать отменённую встречу из своего календаря. Организатору ничего не уйдёт
invite-removed = Встреча удалена из календаря
invite-remove-none = В календаре этой встречи нет
list-meeting-cancelled = Отменена · { $when }
invite-outdated-newer = Приглашение устарело: в календаре более новая версия встречи
invite-outdated-past = Встреча уже прошла
invite-day-loading = Календарь загружается…
invite-conflict = Пересекается: { $title } { $time }
invite-conflict-many = Пересекается: { $title } { $time } и ещё { $n }
invite-no-conflict = Пересечений нет
invite-sample-old-place = Переговорная 2
invite-your-day-title = Ваш день
invite-chip-waiting = Ждёт ответа
invite-chip-accepted = Принято
invite-chip-tentative = Под вопросом
invite-chip-declined = Отклонено
invite-route-short = Ответ уйдёт через календарь сервера
invite-route-short-done = Записано в календарь сервера
invite-side-title = Приглашение
invite-side-strip = Приглашение на встречу · { $when }
invite-side-strip-hint = Подробности и ответ — в панели справа
invite-now = Сейчас { $time }
cal-group-colleagues = Календари коллег
cal-colleague-calendar = Календарь: { $name }
cal-private-other = Частное событие — владелец скрыл подробности
cal-remind-title = Напоминание о встрече
cal-remind-soon = Через { $n } мин: { $title } — { $when }
cal-remind-now = Начинается: { $title } — { $when }
cal-open-colleague = Открыть календарь коллеги
cal-open-placeholder = Имя или адрес сотрудника
cal-open-bad = Выберите сотрудника из подсказок или введите адрес
cal-open-already = Календарь уже открыт: { $name }
cal-open-close = Закрыть календарь
cal-fb-busy = Занят
cal-fb-tentative = Под вопросом
cal-fb-away = Вне офиса
cal-fb-hidden = Занятость скрыта
cal-fb-hidden-tip = Сервер не дал занятость: календарь вам не открыт или сотрудник не найден. Это не «свободен».
cal-fb-only = Видна только занятость: подробности коллега не открыл
cal-fb-some = Видны тема и место: остальное коллега не открыл
cal-chip-colleague = Календарь коллеги
cal-color-change = Цвет календаря

# Правка и отмена своей встречи (S7-07б)
cal-edit-meeting = Изменить
cal-cancel-meeting = Отменить встречу
cal-edit-series-ask = Повторяющаяся встреча — изменить:
cal-cancel-series-ask = Отменить повторяющуюся встречу? Участники получат отмену.
cal-cancel-ask = Отменить встречу? Участники получат отмену.
cal-series-one = Только эту
cal-series-all = Всю серию
cal-cancel-keep = Не отменять
cal-cancel-back = Назад
cal-cancelling = Отменяем встречу…
cal-meeting-cancelled = Встреча отменена, участникам ушла отмена
cal-cancel-failed = Встреча не отменена: { $error }
meeting-title-edit = Изменение встречи
meeting-tab-edit-titled = Изменение встречи · { $title }
meeting-updated = Изменения отправлены: { $title }
# Занятость внешних по ссылке (S8-42)
meeting-by-link = { $role } · по ссылке
meeting-link-change = Ссылка занятости { $email }
meeting-link-set = Указать ссылку
meeting-link-loading = Загружаю занятость по ссылке…
meeting-link-failed = Занятость по ссылке недоступна
meeting-late = начнётся в { $time } — на 5 мин позже
meeting-link-title = Ссылка занятости { $email }
meeting-link-placeholder = https://… — FBURL или календарь .ics
meeting-link-bad = Нужна ссылка https:// с именем сервера
meeting-link-save = Сохранить
meeting-link-remove = Убрать
meeting-link-cancel = Отмена
# Подписки на календари (S8-44)
cal-group-subscriptions = Подписки
cal-sub-open = Добавить по ссылке
cal-sub-placeholder = https://… или webcal://…
cal-sub-add = Добавить
cal-sub-bad = Нужна ссылка https:// или webcal:// с именем сервера
cal-sub-failed = Не обновился, показаны прежние события
cal-sub-refresh = Обновить
cal-sub-remove = Удалить
cal-sub-new = Календарь по ссылке
cal-chip-subscription = Подписка
# Доступ к календарю (S8-43)
cal-share-items = С подробностями встреч
cal-access-busy = Только занятость
cal-access-items = Занятость и подробности
cal-access-close = Закрыть доступ
# Публикация календаря (S8-45)
cal-group-publish = Публикация по ссылке
cal-pub-add-busy = Создать ссылку: только занятость
cal-pub-add-items = Создать ссылку: с подробностями
cal-pub-revoke = Отозвать
cal-pub-copy = Скопировать ссылку
cal-pub-once = Ссылка показана один раз. Её видит любой, кому вы её дадите.
