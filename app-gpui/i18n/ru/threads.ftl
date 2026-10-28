# Беседа в области чтения (S5-05).
thread-toggle = Беседа ({ $n })
thread-quotes-show = Показать цитаты
thread-quotes-hide = Скрыть цитаты
thread-quotes-hidden = … цитата скрыта
thread-loading = загрузка беседы…
thread-me = Я
thread-today = Сегодня
thread-yesterday = Вчера

# Вид беседы: стопка, в списке как Outlook, меню папки (S5-05б).
thread-info = { $n ->
    [one] { $n } письмо
    [few] { $n } письма
   *[many] { $n } писем
} · { $people ->
    [one] { $people } участник
    [few] { $people } участника
   *[many] { $people } участников
}
thread-expand-all = Развернуть все
thread-collapse-all = Свернуть все
thread-to = Кому: { $to }
thread-reply = Ответить
thread-forward = Переслать
thread-quote-show = показать цитату
thread-quote-hide = скрыть цитату
thread-expand = Развернуть беседу
thread-collapse = Свернуть беседу
thread-outlook = { $n ->
    [one] Беседа: { $n } письмо
    [few] Беседа: { $n } письма
   *[many] Беседа: { $n } писем
}
thread-prev = ← пред.
thread-next = след. →
thread-position = { $at } из { $n }
folder-menu-view = Вид беседы
folder-menu-new = Новая папка
folder-menu-rename = Переименовать
folder-new-placeholder = Имя папки
folder-role-fixed = Системную папку нельзя переименовать или перенести
folder-demo = В демо папки не меняются
folder-unsupported = Сервер не умеет менять папки
folder-into-itself = Папку нельзя перенести в саму себя или в её подпапку
folder-other-account = Папку нельзя перенести в другую учётную запись
folder-failed = Папка не изменена: { $error }
thread-view-feed = Лента
thread-view-stack = Стопка
thread-view-outlook = Как Outlook
thread-view-default = По умолчанию
