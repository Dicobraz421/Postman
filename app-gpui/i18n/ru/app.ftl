# Экран почты.
folders-results = 🔍 Результаты ({ $n })
reading-empty = Выберите письмо
reading-from = От:
reading-to = Кому:
reading-date = Дата:
reading-remote-blocked = Внешние картинки не загружаются ({ $n }). Это защищает от отслеживания.
reading-image = [изображение]
reading-image-alt = [изображение: { $alt }]
reading-error = Письмо не показано: { $error }
worker-missing = Письма, картинки и вложения не показываются: не найден компонент разбора писем { $file }. Причина — файл удалён из папки программы или не был распакован из архива. Решение — скачайте { $file } и положите в папку { $dir }; перезапускать Postman не нужно.
worker-missing-letter = Письмо не показано: не хватает компонента программы. Что случилось и как исправить — щёлкните «Нет компонента» в строке состояния.
worker-download = Скачать
status-worker-missing = Нет компонента
status-worker-tip = Письма не показываются — нажмите, чтобы узнать, как исправить
sandbox-full = Песочница: полная
sandbox-partial = Песочница: частичная
sandbox-none = Песочница: нет
sandbox-weak = Песочница разбора писем ослаблена ({ $level }): письма открываются с меньшей защитой.
status-items = Элементов: { $n }
status-accounts = { $n ->
    [one] { $n } аккаунт
    [few] { $n } аккаунта
   *[other] { $n } аккаунтов
}
status-shared = { $n ->
    [one] { $n } общий ящик
    [few] { $n } общих ящика
   *[other] { $n } общих ящиков
}
status-unread = непрочитанных: { $n }
status-cached = (кэш)
status-notifications-hint = Уведомления
second-instance = Postman уже запущен — показано это окно.
# Значок в трее (T3-13).
tray-open = Открыть Postman
tray-quit = Выход
tray-tip-unread = Postman — непрочитанных: { $n }
tray-hint-title = Postman работает в фоне
tray-hint-body = Окно закрыто, почта синхронизируется. Значок — в области уведомлений; выход — в его меню.

# Сбой запуска (журнал Windows и stderr).
fatal-data-dir = каталог данных
fatal-single-instance = проверка, что Postman ещё не запущен
fatal-bench-mailbox = ящик для замера
fatal-screens-dir = каталог снимков
fatal-demo-mailbox = демо-ящик
fatal-store = хранилище
fatal-window = окно
needs-server = Нужна учётная запись на сервере: в демо-режиме это недоступно.
# «Нет у сервера — нет на экране» (S7-03): клавиша команды, которой у сервера нет.
feature-unavailable = Нет у сервера этой учётной записи
# Системные папки (стандартное имя сервера — на языке интерфейса)
folder-role-inbox = Входящие
folder-role-sent = Отправленные
folder-role-drafts = Черновики
folder-role-trash = Удалённые
folder-role-junk = Спам
folder-role-archive = Архив
tab-dirty-tip = Есть несохранённые изменения
thread-count-tip = Писем в беседе
