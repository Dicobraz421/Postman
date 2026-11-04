# Письмо вкладкой, ответ в области чтения, «Перед отправкой» (S5-06б).
compose-external = Внешний
compose-expand = Развернуть
compose-fmt-bold = Ж
compose-fmt-italic = К
compose-fmt-underline = Ч
compose-template = Шаблон
compose-template-empty = Шаблонов пока нет: они появятся в настройках.
compose-signature = Подпись: { $name }
compose-signature-none = без подписи
compose-mark-none = Метка: нет
compose-mark-confidential = Метка: Конфиденциально
compose-mark-hint = Метка видна получателям в Postman; шифрования пока нет.
compose-wait = Ждать ответа
compose-wait-days = { $n ->
    [one] { $n } день
    [few] { $n } дня
   *[many] { $n } дней
}
compose-wait-days-hint = Сколько ждать ответа — щелчок меняет срок
compose-category = Категория
compose-category-none = Без категории
compose-delete-confirm = Удалить черновик?
compose-undo-hint = Отмена отправки { $s } с
compose-undo-suffix = · отмена отправки { $s } с
checks-title = Перед отправкой
checks-count = { $n ->
    [one] { $n } замечание
    [few] { $n } замечания
   *[many] { $n } замечаний
}
checks-none = нет замечаний
checks-clear = Замечаний нет.
checks-card = В тексте похоже на номер банковской карты
checks-card-detail = Утечка платёжных данных: отправка остановлена, пока номер не скрыт.
checks-card-external = Утечка платёжных данных. Письмо уходит внешним получателям.
checks-password = В тексте похоже на пароль
checks-token = В тексте похоже на токен доступа
checks-private-key = В тексте закрытый ключ
checks-secret-detail = Секрет останется у всех получателей: отправка остановлена, пока он не скрыт.
checks-mask = Замаскировать
checks-not-card = Это не карта
checks-not-secret = Это не секрет
checks-attachment = Упомянуто вложение («{ $word }»), а файла нет
checks-attachment-detail = В письме нет вложенных файлов.
checks-attachment-card = Упомянуто вложение — файл не вложен
checks-attach = Вложить файл
checks-ignore = Игнорировать
checks-external = Внешних получателей: { $n } ({ $domains })
checks-external-detail = «Конфиденциально»: письмо уйдёт за пределы компании — зашифрованным сертификатами получателей.
checks-review = Проверить получателей
checks-ok = ОК
checks-moved = { $name } последний раз писал(а) с другого адреса
checks-moved-detail = { $from } → { $to } ({ $when })
checks-replace = Заменить адрес
checks-keep = Оставить
checks-today = сегодня
checks-days-ago = { $n ->
    [one] { $n } день назад
    [few] { $n } дня назад
   *[many] { $n } дней назад
}
checks-no-cert = Нет сертификата: { $names }
checks-no-cert-detail = «Конфиденциально» — письмо шифруется; без сертификата адресат его не прочтёт. Отправить нельзя, пока они в письме.
checks-no-cert-remove = Убрать их
checks-no-cert-unmark = Снять «Конфиденциально»
checks-no-cert-unmark-confirm = Письмо уйдёт без шифрования · Да, снять
checks-split = Уйдут два письма: отдельно — { $names } (ГОСТ)
checks-split-detail = У этих адресатов сертификаты ГОСТ, у остальных — RSA или EC. Оба письма будут в «Отправленных», оба — зашифрованы и вам.
checks-no-cert-find = Найти в каталоге
checks-no-cert-searching = Ищу сертификаты в каталоге…
checks-no-cert-not-in-directory = В каталоге сертификата нет. Без него адресат письмо не прочтёт — уберите адресата или снимите «Конфиденциально».
checks-no-cert-directory-failed = { $reason } Уберите адресата или снимите «Конфиденциально».
