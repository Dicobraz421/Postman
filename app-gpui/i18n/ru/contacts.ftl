# Контакты (S4-02, вид — S5-11): личные контакты (JMAP ContactCard) и справочник сотрудников (Principals).
contacts-account = Контакты
contacts-mine = Мои контакты
contacts-directory = Справочник компании
contacts-nav-note = Справочник приходит с сервера и обновляется сам. Свои правки — в «Моих контактах»: «Изменить» у сотрудника создаёт личную копию.
contacts-search-placeholder = Поиск в контактах
contacts-new = Новый контакт
contacts-loading = Загрузка контактов…
contacts-directory-search-only = Справочник доступен поиском: наберите имя или адрес в поле «Поиск в контактах».
contacts-empty = Здесь пока никого нет.
contacts-nothing = Никого не нашлось
contacts-favorites = Избранное
contacts-frequent = Часто пишу
contacts-frequent-summary = за { $days } дней
contacts-sort-frequency = По частоте
contacts-sort-address = По адресу
contacts-sort-recent = Недавние
contacts-search-title = Поиск
contacts-group-correspondents = Переписка
contacts-kind-correspondent = Адресат переписки
contacts-count = { $n } всего
contacts-found = { $n } найдено · имя, адрес, телефон, организация
contacts-sort-name = По имени
contacts-sort-match = По совпадению
contacts-pick = Выберите контакт
contacts-no-name = (без имени)
contacts-kind = Тип
contacts-kind-contact = Личный контакт
contacts-kind-person = Сотрудник
contacts-kind-room = Переговорная
contacts-kind-resource = Ресурс
contacts-emails = Электронная почта
contacts-organization = Организация
contacts-title = Должность
contacts-write = Написать
contacts-meet = Встреча
contacts-copy = Копировать адрес
contacts-copied = Адрес скопирован: { $email }
contacts-edit = Изменить
contacts-delete = Удалить
contacts-add-mine = Добавить в мои контакты
contacts-badge-mine = Мои контакты
contacts-badge-directory = Из справочника · только чтение
contacts-book-room = Забронировать
contacts-now = сейчас { $time }
contacts-no-titles = названия встреч не видны
contacts-busy-hidden = Занятость скрыта: коллега не открыл вам свой календарь.
contacts-delete-confirm = Удалить контакт «{ $name }»?
contacts-yes = Да
contacts-no = Нет
contacts-readonly = Справочник организации — только просмотр.
contacts-form-new = Новый контакт
contacts-form-edit = Изменить контакт
contacts-form-where = Сохранится в «Мои контакты» на сервере
contacts-form-where-demo = Демо: сохранится только в этом окне
contacts-form-hint = В списках имя показывается как «Фамилия Имя». Несколько адресов или телефонов — через «;». Enter — сохранить, Esc — отмена.
contacts-emails-placeholder = name@example.com; …
contacts-phones-placeholder = +7 …; …
contacts-save = Сохранить
contacts-cancel = Отмена
contacts-saving = Сохранение…
contacts-deleting = Удаление…
contacts-need-name = Укажите имя или адрес.
contacts-bad-email = Неверный адрес: { $email }
contacts-saved = Контакт сохранён: { $name }
contacts-deleted = Контакт удалён: { $name }
contacts-failed = Контакты: { $error }
contacts-demo-local = Демо: контакты меняются только в этом окне, на сервер ничего не уходит.
# Сверка макета T2-G (T2-40…T2-46).
contacts-recent-mail = Последняя переписка
contacts-all-mail = Вся переписка
contacts-no-mail = Переписки пока нет.
contacts-mail-loading = Ищем переписку…
contacts-you = Вы
contacts-lists = Списки рассылки
contacts-lists-all = Все списки
contacts-kind-list = Список рассылки
contacts-list-members = Состав списка сервер не сообщает: письмо на этот адрес получат все участники списка.
contacts-presence-me = это вы
contacts-presence-free = { $f ->
    [yes] свободна
   *[other] свободен
}
contacts-presence-free-long = { $f ->
    [yes] Свободна
   *[other] Свободен
}
contacts-presence-busy = { $f ->
    [yes] занята
   *[other] занят
} до { $time }
contacts-presence-busy-long = { $f ->
    [yes] Занята
   *[other] Занят
} до { $time }
contacts-presence-away = вне офиса до { $time }
contacts-presence-away-long = Вне офиса до { $time }
contacts-presence-hidden = занятость скрыта
contacts-presence-date = { $day } { $month }
contacts-window = · ближайшее окно { $from }–{ $to }
contacts-meet-at = Встреча в { $time }
contacts-manager = Руководитель
contacts-office = Кабинет
contacts-department = Отдел
contacts-work-phone = Рабочий телефон
contacts-mobile = Мобильный
contacts-last-name = Фамилия
contacts-first-name = Имя
contacts-book = Адресная книга
contacts-layout-fixed = раскладка исправлена: { $text }
contacts-layout-reset = Сбросить поиск
contacts-directory-updated = Справочник обновлён { $time }
# Личные группы (T2-47).
contacts-groups = Группы
contacts-groups-all = Все группы
contacts-new-group = Новая группа
contacts-kind-group = Группа
contacts-badge-group = Моя группа
contacts-group-count = { $n ->
    [one] { $n } участник
    [few] { $n } участника
   *[many] { $n } участников
}
contacts-group-summary = Группа · { $n ->
    [one] { $n } участник
    [few] { $n } участника
   *[many] { $n } участников
}
contacts-group-members = Участники
contacts-group-empty = В группе пока никого: «Изменить» — добавить участников, или «В группу» у контакта.
contacts-group-remove = Убрать из группы
contacts-group-foreign = Участник из другой программы
contacts-group-hint = Письмо группе уходит каждому участнику: получатели видят адреса участников, а не группу. Групповой адрес на сервере — это «Списки рассылки».
contacts-write-group = Написать группе
contacts-group-form-new = Новая группа
contacts-group-form-edit = Изменить группу
contacts-group-form-where = Сохранится в «Мои контакты» на сервере
contacts-group-form-hint = Участники — через «;»: адрес, «Имя <адрес>» или имя из справочника и контактов. Enter — сохранить, Esc — отменить.
contacts-group-name = Название
contacts-group-name-placeholder = Например, Руководители отделов
contacts-group-members-placeholder = адреса или имена через «;»
contacts-group-need-name = Укажите название группы
contacts-group-saved = Группа сохранена: { $name }
contacts-group-delete-confirm = Удалить группу «{ $name }»? Контакты останутся.
contacts-to-group = В группу
contacts-to-group-new = Новая группа…
contacts-added-to-group = { $name } — в группе «{ $group }»
contacts-already-in-group = { $name } уже в группе «{ $group }»
contacts-removed-from-group = { $name } — больше не в группе «{ $group }»
