# Вложения: лента под шапкой, встроенный просмотр, макросы (S5-07).
att-ribbon = Вложения · { $n } · { $size }
att-save-all = Сохранить все…
att-more = +{ $n }
att-less = Свернуть
att-open = Открыть
att-save = Сохранить как…
att-preview = Просмотр
att-loading = скачивание…
att-status-macros = Содержит макросы · открытие заблокировано
att-status-blocked = Исполняемый файл · только сохранить
att-status-archive = Архив
att-saved-all = { $n ->
    [one] Сохранён { $n } файл: { $dir }
    [few] Сохранено { $n } файла: { $dir }
   *[many] Сохранено { $n } файлов: { $dir }
}
att-save-all-failed = «{ $name }» не сохранён: { $error }

att-pv-close = Закрыть просмотр
att-pv-note-builtin = Встроенный просмотр · без запуска внешних программ
att-pv-note-archive = Содержимое архива · файлы не распаковываются на диск
att-pv-note-check = Проверка вложения · файл не открывался
att-pv-loading = Файл разбирается в песочнице…
att-pv-failed = Просмотр не удался: { $error }
att-pv-too-big = Файл больше { $size } — встроенный просмотр недоступен, файл можно сохранить.
att-pv-page = { $page } / { $pages }
att-pv-prev-page = Предыдущая страница
att-pv-next-page = Следующая страница
att-pv-image-info = { $w } × { $h } px · { $format }
att-pv-open-app = Открыть в приложении
att-pv-save = Сохранить…
att-pv-col-name = Имя
att-pv-col-size = Размер
att-pv-col-date = Изменён
att-pv-locked = зашифрован
att-pv-archive-total = { $n ->
    [one] { $n } файл · { $size } после распаковки
    [few] { $n } файла · { $size } после распаковки
   *[many] { $n } файлов · { $size } после распаковки
}
att-pv-archive-more = ещё { $n } в архиве
att-pv-archive-safe = Исполняемых файлов нет
att-pv-archive-danger = { $n ->
    [one] { $n } опасный файл: исполняемый или с макросами
    [few] { $n } опасных файла: исполняемые или с макросами
   *[many] { $n } опасных файлов: исполняемые или с макросами
}
att-pv-archive-encrypted = Есть зашифрованные файлы — их содержимое не проверить
att-pv-archive-bomb = Сжатие подозрительно сильное (возможна zip-бомба) — не распаковывайте
att-pv-text-cut = Показано начало файла — { $size }
att-pv-doc-cut = Показано начало документа
att-pv-none = Встроенного просмотра для этого типа файла нет.
att-pv-office-clean = Макросов не найдено. Содержимое документа встроенный просмотр не показывает.
att-pv-blocked = Открытие заблокировано
att-pv-macros = Содержит макросы
att-pv-exe = Исполняемый файл или скрипт
att-pv-found = Что найдено
att-pv-found-macros = { $n ->
    [one] VBA-макросы: { $n } модуль
    [few] VBA-макросы: { $n } модуля
   *[many] VBA-макросы: { $n } модулей
}
att-pv-found-project = Проект VBA (модули не перечислены)
att-pv-found-type = Тип файла с макросами ({ $ext })
att-pv-found-exe = Файл запускается как программа ({ $ext })
att-pv-why = Почему заблокировано
att-pv-why-macros = Макросы выполняют код на компьютере. Из почты такие файлы не открываются.
att-pv-why-exe = Программы и скрипты из почты не запускаются.
att-pv-save-warning = Файл сохранится с пометкой «из Интернета»: Office откроет его в защищённом просмотре, макросы будут отключены.
att-pv-save-off = Сохранение файлов с макросами выключено (Настройки → Безопасность).

# Перетаскивание вложения в другое приложение (T1-18).
att-drag-ready = «{ $name }» загружено — перетащите его ещё раз
att-drag-failed = Не удалось перетащить «{ $name }»: { $error }
