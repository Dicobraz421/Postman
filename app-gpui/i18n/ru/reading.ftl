# Область чтения: вложения и приглашения (S3-03).
reading-att-size = { $unit ->
    [b] { $n } Б
    [kb] { $n } КБ
   *[mb] { $n } МБ
}
reading-saved = Сохранено: { $path }
reading-file-failed = «{ $name }»: { $error }
reading-blob-failed = Вложение не скачано: { $error }
reading-blob-unavailable = вложение недоступно без сервера

reading-invite-loading = 📅 Приглашение на встречу…
reading-invite-failed-parse = 📅 Приглашение не разобрано: { $error }
reading-invite-head = { $method ->
    [CANCEL] 📅 Встреча отменена
    [REPLY] 📅 Ответ на приглашение
    [COUNTER] 📅 Предложено другое время
   *[other] 📅 Приглашение на встречу
}
reading-invite-when = Когда:
reading-invite-where = Где:
reading-invite-organizer = Организатор:
reading-invite-attendees = Участники:
reading-invite-all-day = { $day }, весь день
reading-invite-not-invited = Вас нет среди участников — ответить нельзя.
reading-invite-accept = ✔ Принять
reading-invite-tentative = ? Под вопросом
reading-invite-decline = ✖ Отклонить
reading-invite-sending = Отправка ответа…
reading-invite-mine = Ваш ответ: { $status }
reading-invite-sent-server = Ответ на приглашение отправлен через календарь.
reading-invite-sent-mail = Ответ отправлен организатору.
reading-invite-silent-saved = Ответ записан в ваш календарь, организатору не отправлен.
reading-invite-silent = Организатору ответ не отправлен.
reading-threat = Письмо помечено как фишинг. Не открывайте ссылки и вложения, если не уверены в отправителе.
reading-invite-sent-imported = Ответ отправлен организатору, встреча добавлена в календарь.
reading-invite-failed = Ответ на приглашение не отправлен: { $error }
reading-partstat = { $s ->
    [accepted] принято
    [tentative] под вопросом
    [declined] отклонено
    [delegated] передано
   *[needs-action] нет ответа
}
reading-weekday = { $d ->
    [0] пн
    [1] вт
    [2] ср
    [3] чт
    [4] пт
    [5] сб
   *[6] вс
}
reading-link-mismatch = Текст ссылки — «{ $shown }», а ведёт она на { $host }. Перейти по настоящему адресу?
reading-link-go = Перейти
reading-quick-reply = Ответить — { $name }
reading-quick-reply-tip = Ответить
