# Письмо: вкладка и ответ в области чтения (S3-01, S5-06б).
compose-title-new = Новое письмо
compose-send = Отправить
compose-attach = Вложить
compose-to = Кому
compose-cc = Копия
compose-bcc = Скрытая
compose-subject = Тема
compose-attachments = Вложения
compose-attachments-empty = Файлов нет — «Вложить» или перетащите файлы в письмо.
compose-to-placeholder = Имя или адрес — подсказки из адресной книги
compose-cc-placeholder = Копия
compose-bcc-placeholder = Скрытая копия: адресаты не видят друг друга
compose-subject-placeholder = Тема письма
compose-body-placeholder = Текст письма (Markdown: **жирный**, *курсив*, - список, > цитата). Файлы можно перетащить в письмо.
compose-size-b = { $n } Б
compose-size-kb = { $n } КБ
compose-size-mb = { $n } МБ
compose-sending = Отправка…
compose-saving = Сохранение черновика…
compose-autosaving = Автосохранение…
compose-saved = Черновик сохранён { $time }
compose-bad-addresses = Не распознаны адреса: { $list }
compose-no-recipients = Не указан ни один получатель.
compose-blocked = Отправка остановлена: снимите красное замечание «Перед отправкой».
compose-no-server-send = Не отправлено: нет связи с сервером (демо-режим). Письмо осталось как было.
compose-no-server-save = Черновик не сохранён: нет связи с сервером (демо-режим). Текст остался в письме.
compose-failed = Ошибка: { $error }
compose-attach-failed = «{ $name }» не вложен: { $error }
compose-large = Вложения: { $size } — сервер может не принять письмо.
compose-confirm-close = Сохранить черновик?
compose-confirm-yes = Да
compose-confirm-no = Нет
compose-confirm-cancel = Отмена
# Подпись S/MIME (S5-12 Ф3).
compose-sign = Подписать
compose-sign-on = Письмо будет подписано сертификатом адреса «От» — снять
compose-sign-off = Подписать письмо сертификатом адреса «От»
compose-sign-need-pin = Не подписано: нужен PIN смарт-карты · снимите «Подписать» или вставьте карту и повторите
compose-sign-no-card = Не подписано: смарт-карта не найдена · вставьте карту и повторите
compose-sign-cancelled = Не подписано: ввод PIN отменён
compose-sign-need-skzi = Не подписано: сертификат ГОСТ, нужно сертифицированное СКЗИ (КриптоПро CSP или ViPNet CSP)
compose-sign-no-cert = Не подписано: нет сертификата подписи для { $address } · снимите «Подписать»
compose-sign-eight-bit = Не подписано: письмо не в 7 бит
compose-sign-failed = Не подписано: { $error }
# Шифрование S/MIME (S5-12 Ф4).
compose-seal-no-cert = Не отправлено: нет сертификата шифрования для { $address }
compose-seal-no-own-cert = Не отправлено: нет своего сертификата шифрования (письмо — себе тоже)
compose-seal-need-skzi = Не отправлено: адресаты с ГОСТ — нужно сертифицированное СКЗИ (КриптоПро CSP или ViPNet CSP)
compose-seal-failed = Не зашифровано: { $error }

# Переслать вложениями (T1-17).
compose-attached-loading = Загружаю письма для пересылки вложениями…
compose-attached-none = Не удалось переслать: писем нет на сервере
compose-attached-skipped = { $n ->
    [one] { $n } письмо не вложено: его нет на сервере
    [few] { $n } письма не вложено: их нет на сервере
   *[many] { $n } писем не вложено: их нет на сервере
}
