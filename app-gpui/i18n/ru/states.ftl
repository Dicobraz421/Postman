# Состояния (S5-08): без связи, стопка уведомлений и их история.
states-offline-retry = Нет связи · повтор через { $n } с
states-offline-trying = Нет связи · подключение…
states-offline-queued = { $n ->
    [one] { $n } изменение ждёт отправки
    [few] { $n } изменения ждут отправки
   *[many] { $n } изменений ждут отправки
}
states-banner = Нет подключения к { $host }. Показаны письма из кэша; отметки, перемещения и удаления уйдут на сервер, когда связь вернётся.
states-banner-queued = { $n ->
    [one] Нет подключения к { $host }. Показаны письма из кэша; { $n } изменение уйдёт на сервер, когда связь вернётся.
    [few] Нет подключения к { $host }. Показаны письма из кэша; { $n } изменения уйдут на сервер, когда связь вернётся.
   *[many] Нет подключения к { $host }. Показаны письма из кэша; { $n } изменений уйдут на сервер, когда связь вернётся.
}
states-retry = Повторить сейчас
states-banner-hide = Скрыть сообщение о сети
states-account-offline = нет связи
states-account-offline-hint = Нет связи с сервером — показан кэш
states-lost = Нет подключения к { $host } — работаем офлайн
states-back = Связь с { $host } восстановлена

notices-title = Уведомления
notices-kept = За этот запуск Postman
notices-all = Все
notices-errors = Ошибки
notices-actions = Действия
notices-today = Сегодня
notices-yesterday = Вчера
notices-done = выполнено
notices-empty = Уведомлений нет.
notices-log = Подробности ошибок — в журнале Windows «Приложение» (источник Postman).
notices-left = { $n } с
notices-more = +{ $n } ещё
newmail-title = Новых писем: { $n }
newmail-more = и ещё { $n }
