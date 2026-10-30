# Учётная запись и живой ящик (S3-04).
signin-url-placeholder = https://mail.example.com
signin-user-placeholder = user@example.com
signin-busy = Подключение…
signin-fill = Заполните адрес сервера, логин и пароль.
signin-failed = Вход не выполнен: { $error }
account-sync-error = Синхронизация: { $error }
account-sync-stopped = Нет связи с сервером: синхронизация остановлена, работа — с локальной копией.
account-remember-failed = Вход выполнен, но пароль не сохранён: { $error }
# Добавление аккаунта на месте и первый запуск (S5-09).
signin-title = Добро пожаловать в Postman
signin-hint = Почта, календарь и контакты сервера Stalwart. Работает без сети с локальной копией, ничего не отправляет третьим лицам.
signin-new = Новый аккаунт
signin-new-hint = — настраивается на месте, окно не открывается
signin-step-server = Сервер
signin-step-login = Логин
signin-step-password = Пароль
signin-url = Адрес сервера
signin-url-hint = Например, mail.example.com — Postman найдёт настройки JMAP сам (/.well-known/jmap). Почта по IMAP — imaps://imap.example.com (SMTP — тот же узел или ?smtp=smtp.example.com; календарь и контакты — CalDAV и CardDAV того же узла, если они там есть, или &dav=https://dav.example.com). Exchange — домен почты или адрес EWS и вход адресом почты: EWS Postman найдёт сам.
signin-user = Логин
signin-user-hint = Обычно — адрес почты.
signin-password = Пароль
signin-store = Хранение пароля
signin-store-windows = Хранить: Диспетчер учётных данных Windows
signin-store-never = Не хранить — спрашивать при запуске
signin-store-note = Postman сам секреты не сохраняет — ни на диск, ни в настройки.
signin-tls = Соединение: TLS 1.2 или 1.3, более старые версии не поддерживаются. Сертификат сервера и его отзыв проверяет Windows.
signin-tls-insecure = Сертификат сервера не проверяется (POSTMAN_INSECURE_TLS — только для стенда).
signin-change = Изменить
signin-next = Продолжить
signin-back = Назад
signin-cancel = Отмена
signin-submit = Проверить и подключить
signin-fill-url = Укажите адрес сервера.
signin-fill-user = Укажите логин.
signin-bad-url = Адрес сервера не похож на адрес: mail.example.com или https://mail.example.com.
signin-demo = Без сервера — запуск с --demo · Ctrl+, — настройки
# Вход учётной записью Windows (T1-02б).
signin-windows = Войти как пользователь Windows (без пароля)
signin-with-password = Войти с паролем
signin-windows-hint = Exchange в домене: Postman войдёт учётной записью, с которой вы вошли в Windows. Пароль не спрашивается и не хранится.
signin-windows-failed = Не удалось войти учётной записью Windows — введите пароль.
