# «Экспорт…» (S8-32, макет S8-D «Экспорт»): полоса выбора, ход, итог.
export-menu = Экспорт…
export-what-folder = { $n ->
    [one] Экспорт «{ $folder }» · { $count } письмо
    [few] Экспорт «{ $folder }» · { $count } письма
   *[many] Экспорт «{ $folder }» · { $count } писем
}
export-what-selection = { $n ->
    [one] Экспорт: { $count } письмо
    [few] Экспорт: { $count } письма
   *[many] Экспорт: { $count } писем
}
export-what-account = { $n ->
    [one] Экспорт учётки «{ $account }» · { $count } письмо
    [few] Экспорт учётки «{ $account }» · { $count } письма
   *[many] Экспорт учётки «{ $account }» · { $count } писем
}
export-what-all = { $n ->
    [one] Экспорт всего · { $count } письмо
    [few] Экспорт всего · { $count } письма
   *[many] Экспорт всего · { $count } писем
}
export-subfolders = С подпапками ({ $n })
export-start = Выбрать папку и начать…
export-cancel = Отмена
export-eml = Файл на письмо · Outlook, Thunderbird, Apple Mail
export-mbox = Файл на папку · Thunderbird, Apple Mail
export-maildir = Каталог на папку · mutt, Dovecot
export-msg = Файл на письмо · Outlook: перетащить в папку
export-pst = Файл данных Outlook · «Импорт и экспорт»
export-running = Экспорт в { $format }
export-fetching = загрузка с сервера: { $n }
export-status = Экспорт: { $done } из { $total }
export-stop = Остановить
export-done = { $n ->
    [one] Экспортировано { $count } письмо
    [few] Экспортировано { $count } письма
   *[many] Экспортировано { $count } писем
}
export-done-partial = Экспортировано { $done } из { $total }
export-failed-fetch = { $n ->
    [one] { $count } письмо не загрузилось с сервера «{ $server }»
    [few] { $count } письма не загрузились с сервера «{ $server }»
   *[many] { $count } писем не загрузились с сервера «{ $server }»
}
export-stopped = Экспорт остановлен: записано { $done } из { $total }
export-stopped-detail = Готовые файлы целы, недописанный удалён
export-open = Открыть папку
