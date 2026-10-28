# Выделение и массовые действия (S5-08): флажки, панель над списком, сводка, контекстное меню, «Сообщить о фишинге».
bulk-pick = Выбрать (Ctrl+щелчок, Shift+щелчок — диапазон)
bulk-select-all = Выделить все
bulk-selected = Выбрано: { $n }
bulk-of = из { $total }
bulk-clear-hint = Снять выделение
bulk-read = Прочитано
bulk-unread = Не прочитано
bulk-flag = Флаг
bulk-unflag = Снять флаг
bulk-archive = Архив
bulk-forward-attached = Переслать
bulk-delete = Удалить
bulk-uncategorize = Снять категорию
bulk-move-plain = В папку
bulk-category-plain = Категория
bulk-due-plain = Срок
bulk-phishing = Сообщить о фишинге
bulk-letters = { $n ->
    [one] { $n } письмо
    [few] { $n } письма
   *[many] { $n } писем
}
bulk-read-only = Не изменено: папка общего ящика открыта только для чтения.
bulk-nothing = Менять нечего: у писем это уже так.
bulk-skipped = { $n ->
    [one] { $n } письмо пропущено: нет прав в общем ящике или другой аккаунт.
    [few] { $n } письма пропущено: нет прав в общем ящике или другой аккаунт.
   *[many] { $n } писем пропущено: нет прав в общем ящике или другой аккаунт.
}
bulk-undo = Отменить
bulk-undone = Отменено · письма возвращены
bulk-done-read = { $n ->
    [one] Прочитано · { $n } письмо
    [few] Прочитано · { $n } письма
   *[many] Прочитано · { $n } писем
}
bulk-done-unread = { $n ->
    [one] Не прочитано · { $n } письмо
    [few] Не прочитано · { $n } письма
   *[many] Не прочитано · { $n } писем
}
bulk-done-flag = { $n ->
    [one] С флагом · { $n } письмо
    [few] С флагом · { $n } письма
   *[many] С флагом · { $n } писем
}
bulk-done-unflag = { $n ->
    [one] Флаг снят · { $n } письмо
    [few] Флаг снят · { $n } письма
   *[many] Флаг снят · { $n } писем
}
bulk-done-category = { $n ->
    [one] Категория «{ $name }» · { $n } письмо
    [few] Категория «{ $name }» · { $n } письма
   *[many] Категория «{ $name }» · { $n } писем
}
bulk-done-uncategorize = { $n ->
    [one] Категории сняты · { $n } письмо
    [few] Категории сняты · { $n } письма
   *[many] Категории сняты · { $n } писем
}
bulk-done-move = { $n ->
    [one] Перемещено в «{ $folder }» · { $n } письмо
    [few] Перемещено в «{ $folder }» · { $n } письма
   *[many] Перемещено в «{ $folder }» · { $n } писем
}
bulk-done-archive = { $n ->
    [one] Перемещено в «Архив» · { $n } письмо
    [few] Перемещено в «Архив» · { $n } письма
   *[many] Перемещено в «Архив» · { $n } писем
}
bulk-done-trash = { $n ->
    [one] Перемещено в «Удалённые» · { $n } письмо
    [few] Перемещено в «Удалённые» · { $n } письма
   *[many] Перемещено в «Удалённые» · { $n } писем
}
bulk-done-phishing = { $n ->
    [one] Сообщено о фишинге · { $n } письмо в «Нежелательных»
    [few] Сообщено о фишинге · { $n } письма в «Нежелательных»
   *[many] Сообщено о фишинге · { $n } писем в «Нежелательных»
}
bulk-done-phishing-marked = { $n ->
    [one] Сообщено о фишинге · { $n } письмо помечено угрозой
    [few] Сообщено о фишинге · { $n } письма помечено угрозой
   *[many] Сообщено о фишинге · { $n } писем помечено угрозой
}
bulk-done-purge = { $n ->
    [one] Удалено навсегда · { $n } письмо
    [few] Удалено навсегда · { $n } письма
   *[many] Удалено навсегда · { $n } писем
}
bulk-purge-confirm = { $n ->
    [one] Удалить навсегда { $n } письмо из «Удалённых»?
    [few] Удалить навсегда { $n } письма из «Удалённых»?
   *[many] Удалить навсегда { $n } писем из «Удалённых»?
}
bulk-phishing-subject = { $n ->
    [one] Фишинг: { $n } письмо
    [few] Фишинг: { $n } письма
   *[many] Фишинг: { $n } писем
}
bulk-phishing-text = Пользователь сообщил о фишинге. Письма — во вложении (.eml), как пришли.
bulk-phishing-sent = Сообщение о фишинге отправлено на { $to }
bulk-phishing-failed = Сообщение о фишинге не отправлено: { $error }
bulk-size-kb = { $n } КБ
bulk-size-mb = { $n } МБ
bulk-senders-more = { $who } и ещё { $n }
bulk-summary-title = { $n ->
    [one] { $n } письмо выбрано
    [few] { $n } письма выбрано
   *[many] { $n } писем выбрано
}
bulk-summary-hint = Действия применятся ко всем выбранным
bulk-summary-from = От: { $who }
bulk-summary-unread = непрочитанных: { $n }
bulk-summary-more = и ещё { $n }
bulk-unpick = Убрать из выделения
bulk-summary-clear = Снять

# Контекстное меню письма.
menu-reply = Ответить
menu-reply-all = Ответить всем
menu-forward = Переслать
menu-forward-attached = Переслать вложениями
menu-read = Отметить прочитанным
menu-unread = Отметить непрочитанным
menu-flag = Пометить флагом
menu-unflag = Снять флаг
menu-category = Категория
menu-due = Срок…
menu-remind = Напомнить…
menu-wait = Ждать ответа…
menu-expire = Исчезнет…
menu-move = Переместить
menu-archive = Архив
menu-delete = Удалить
menu-phishing = Сообщить о фишинге
menu-on-selection = { $n ->
    [one] Выбрано { $n } письмо
    [few] Выбрано { $n } письма
   *[many] Выбрано { $n } писем
}
