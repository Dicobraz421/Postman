# Поиск в командной строке (S5-10): заголовок результатов, чипы «Уточнить», совпадения в письме.
search-title = Поиск: { $query }
search-summary = { $n ->
    [one] { $n } результат
    [few] { $n } результата
   *[many] { $n } результатов
} · { $place }
search-summary-waiting = { $n ->
    [one] { $n } результат
    [few] { $n } результата
   *[many] { $n } результатов
} · { $place } · ищем на сервере…
search-where-all = все папки
search-close = Закрыть поиск — к папке
search-refine = Уточнить
search-refine-from = от: { $name }
search-refine-to-me = кому: мне
search-refine-attachment = есть: вложения
search-refine-unread = непрочитанные
search-refine-within = за { $n ->
    [one] { $n } день
    [few] { $n } дня
   *[many] { $n } дней
}
search-refine-category = категория: { $name }
search-empty = Ничего не найдено. Уберите уточнения или поищите везде.
search-matches = Совпадение { $k } из { $n }
search-matches-none = Совпадения: нет в тексте
search-matches-query = «{ $query }»
search-refine-only = только { $folder }
search-clear-refines = Сбросить уточнения
