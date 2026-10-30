# Перенос между учётками (S8-16, макет S8-D «Перенос»): вопрос, ход, пауза, нет места, остановка, итог, значки строки.
migration-ask = { $n ->
    [one] { $count } письмо перенесено из «{ $from }»
    [few] { $count } письма перенесены из «{ $from }»
   *[many] { $count } писем перенесены из «{ $from }»
}
migration-ask-more = { $n ->
    [one] { $count } письмо перенесено из «{ $from }» и ещё { $more } папки
    [few] { $count } письма перенесены из «{ $from }» и ещё { $more } папки
   *[many] { $count } писем перенесены из «{ $from }» и ещё { $more } папки
}
migration-ask-exchange = · Метки Postman на Exchange не сохранятся
migration-to-server = Перенести на сервер «{ $server }»
migration-keep-local = Оставить локальным архивом
migration-running = Перенос в «{ $to }»
migration-running-detail = · { $done } из { $total } · { $bytes } из { $total_bytes }
migration-running-eta = · осталось около { $eta }
migration-paused = Перенос на паузе
migration-pause = Пауза
migration-resume = Продолжить
migration-stop = Остановить
migration-of = из { $total }
migration-no-space = { $n ->
    [one] На сервере «{ $server }» нет места: осталось { $count } письмо · { $bytes }
    [few] На сервере «{ $server }» нет места: осталось { $count } письма · { $bytes }
   *[many] На сервере «{ $server }» нет места: осталось { $count } писем · { $bytes }
}
migration-keep-local-short = Оставить локально
migration-stop-ask = Остановить перенос: готово { $done } из { $total }
migration-stop-keep = Оставить остальные локально
migration-stop-return = Вернуть на место
migration-stop-no = Не останавливать
migration-status = Перенос: { $done } из { $total } · { $bytes }
migration-status-paused = Перенос на паузе: { $done } из { $total }
migration-status-no-space = Перенос стоит: нет места
migration-done = { $n ->
    [one] Перенесено на сервер «{ $server }»: { $count } письмо
    [few] Перенесено на сервер «{ $server }»: { $count } письма
   *[many] Перенесено на сервер «{ $server }»: { $count } писем
}
migration-done-source = У «{ $source }» письма удалены после ответа «{ $server }»
migration-done-partial = Перенесено на «{ $server }»: { $done } из { $total }
migration-done-local = { $n ->
    [one] { $count } письмо — только на этом компьютере
    [few] { $count } письма — только на этом компьютере
   *[many] { $count } писем — только на этом компьютере
}
migration-done-local-detail = «{ $to }», у «{ $source }» удалены
migration-returned = { $n ->
    [one] { $count } письмо возвращено в «{ $from }»
    [few] { $count } письма возвращены в «{ $from }»
   *[many] { $count } писем возвращены в «{ $from }»
}
migration-source-kept = { $n ->
    [one] { $n } письмо не удалено у источника: сервер отказал, у цели оно с пометкой
    [few] { $n } письма не удалены у источника: сервер отказал, у цели они с пометкой
   *[many] { $n } писем не удалены у источника: сервер отказал, у цели они с пометкой
}
migration-core-stopped = Перенос недоступен: поток хранилища остановился. Причина — в журнале.
migration-row-pending = Ждёт переноса на сервер «{ $server }»
migration-row-paused = Перенос на паузе
migration-row-no-space = Ждёт места на сервере «{ $server }»
migration-row-local = Только на этом компьютере — на сервере «{ $server }» его нет
migration-row-too-large = Только на этом компьютере: { $size } — больше предела «{ $server }»
migration-size-gb = { $n } ГБ
migration-eta-min = { $n } мин
migration-eta-hours = { $h } ч { $m } мин
