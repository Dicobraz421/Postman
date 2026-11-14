# Каналы бесед: Telegram в «Настройки → Аккаунты» (CH-03).
tg-channels = Каналы бесед
tg-channel = Telegram
tg-channel-hint = Беседы Telegram рядом с почтой. Канал в разработке — по умолчанию выключен.
tg-policy = Задано политикой организации.
tg-add = Добавить учётку Telegram
tg-no-key = В этой сборке нет ключа приложения Telegram: вход недоступен.
tg-sign-out = Выйти
tg-state-online = В сети
tg-state-connecting = Подключение…
tg-state-offline = Нет связи, повтор через { $secs } с
tg-signin-title = Вход в Telegram
tg-connecting = Подключение к Telegram…
tg-wait = Ждём ответа Telegram…
tg-phone = Номер телефона
tg-phone-hint = С кодом страны, например +7 900 000-00-00.
tg-get-code = Получить код
tg-use-qr = Войти по QR-коду
tg-use-phone = Войти по номеру телефона
tg-qr-hint = Откройте Telegram на телефоне: «Настройки» → «Устройства» → «Подключить устройство» — и наведите камеру на код.
tg-code = { $via ->
    [app] Код из приложения Telegram
    [sms] Код из SMS
    [call] Код из звонка
    [email] Код из письма
   *[other] Код
}
tg-code-length = Цифр в коде: { $n }.
tg-resend = Отправить код снова
tg-sign-in = Войти
tg-password = Пароль двухэтапной проверки
tg-password-hint = Подсказка: { $hint }
tg-err-phone = Telegram не принял номер: проверьте его и код страны.
tg-err-code = Неверный код: проверьте и введите снова.
tg-err-code-expired = Код устарел: отправьте его снова.
tg-err-password = Неверный пароль двухэтапной проверки.
tg-err-sign-up = Этот номер не зарегистрирован в Telegram: зарегистрируйтесь в официальном приложении.
tg-err-denied = Telegram отказал во входе.
tg-err-wait = Telegram просит подождать { $secs } с.
tg-err-network = Нет связи с Telegram: проверьте сеть и повторите.
tg-err-signed-out = Вход истёк: войдите снова.
tg-err-other = Не удалось войти в Telegram.
