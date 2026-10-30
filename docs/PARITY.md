# Соответствие classic Outlook

> Генерируется из реестра команд (`app-gpui/src/commands.rs`) тестом `parity_doc_is_current`.
> Руками не править. Обновить: `POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui parity`.
> «есть» — у действия команды есть обработчик на экране; доля сделанного только растёт (тест).

Сделано: 136 из 136 (100 %).

Хоткей — схема «Outlook» (по умолчанию); «Одиночные» — буквы схемы «Одиночные клавиши» (S5-17) поверх неё.

| Функция | Команда | Хоткей | Одиночные | Статус |
|---|---|---|---|---|
| Следующее письмо | `mail.next` | `down` | `j` | есть |
| Предыдущее письмо | `mail.prev` | `up` | `k` | есть |
| Первое письмо | `mail.first` | `home` |  | есть |
| Последнее письмо | `mail.last` | `end` |  | есть |
| Страница вниз | `mail.page_down` | `pagedown` |  | есть |
| Страница вверх | `mail.page_up` | `pageup` |  | есть |
| Отметить как прочитанное | `mail.mark_read` | `ctrl-q` | `q` | есть |
| Отметить как непрочитанное | `mail.mark_unread` | `ctrl-u` | `u` | есть |
| Флаг | `mail.toggle_flag` |  |  | есть |
| Категория | `mail.categorize` | `1`, `2`, `3`, `4`, `5`, `6`, `7`, `8`, `9` |  | есть |
| Создать письмо | `mail.new` | `ctrl-n` | `c` | есть |
| Ответить | `mail.reply` | `ctrl-r` | `r` | есть |
| Ответить всем | `mail.reply_all` | `ctrl-shift-r` | `a` | есть |
| Переслать | `mail.forward` | `ctrl-f` | `f` | есть |
| Переслать вложениями | `mail.forward_attached` |  |  | есть |
| Удалить | `mail.delete` | `delete` |  | есть |
| Срок… | `mail.due` | `insert` |  | есть |
| Напомнить… | `mail.remind` |  |  | есть |
| Ждать ответа… | `mail.wait` |  |  | есть |
| Исчезнет… | `mail.expire` |  |  | есть |
| Выделить все | `mail.select_all` | `ctrl-a` |  | есть |
| Снять выделение | `mail.clear_selection` |  |  | есть |
| Расширить выделение вниз | `mail.select_down` | `shift-down` |  | есть |
| Расширить выделение вверх | `mail.select_up` | `shift-up` |  | есть |
| Контекстное меню | `mail.context_menu` | `shift-f10`, `menu` |  | есть |
| Переместить в папку… | `mail.move` | `ctrl-shift-v` | `v` | есть |
| Архив | `mail.archive` | `backspace` | `e` | есть |
| Сообщить о фишинге | `mail.report_phishing` |  |  | есть |
| Подробности подписи | `mail.signature` |  |  | есть |
| Взять в работу | `mail.take` | `ctrl-j` |  | есть |
| Беседа: лента вместо письма | `thread.toggle` |  |  | есть |
| Беседа: показать цитаты | `thread.quotes` |  |  | есть |
| Следующее письмо беседы | `thread.next` | `ctrl-right` |  | есть |
| Предыдущее письмо беседы | `thread.prev` | `ctrl-left` |  | есть |
| Беседа: свернуть или развернуть | `thread.fold` | `enter` |  | есть |
| Беседа: развернуть | `thread.expand` |  |  | есть |
| Беседа: свернуть | `thread.collapse` |  |  | есть |
| Просмотр вложения | `attach.preview` |  |  | есть |
| Сохранить все вложения | `attach.save_all` |  |  | есть |
| Следующее вложение (в просмотре) | `attach.next` | `right` |  | есть |
| Предыдущее вложение (в просмотре) | `attach.prev` | `left` |  | есть |
| Почта | `view.mail` | `ctrl-1` |  | есть |
| Календарь | `view.calendar` | `ctrl-2` |  | есть |
| Контакты | `view.contacts` | `ctrl-3` |  | есть |
| Задачи | `view.tasks` | `ctrl-4` |  | есть |
| Новая вкладка почты | `tab.new` | `ctrl-t` |  | есть |
| Следующая вкладка | `tab.next` | `ctrl-tab` |  | есть |
| Предыдущая вкладка | `tab.prev` | `ctrl-shift-tab` |  | есть |
| Закрыть вкладку | `tab.close` | `ctrl-w` |  | есть |
| Уведомления | `view.notifications` |  |  | есть |
| Поиск | `search.focus` | `ctrl-e` | `/` | есть |
| Сбросить поиск | `search.clear` | `escape` |  | есть |
| Поиск: везде | `search.all` |  |  | есть |
| Поиск: в текущей папке | `search.folder` |  |  | есть |
| Поиск: только с вложениями | `search.attachments` |  |  | есть |
| Поиск: только непрочитанные | `search.unread` |  |  | есть |
| Поиск: за 30 дней | `search.recent` |  |  | есть |
| Следующее совпадение в письме | `search.next_match` | `f3` |  | есть |
| Предыдущее совпадение в письме | `search.prev_match` | `shift-f3` |  | есть |
| Палитра команд | `palette.open` | `ctrl-k` |  | есть |
| Палитра: следующая строка | `palette.next` | `down` |  | есть |
| Палитра: предыдущая строка | `palette.prev` | `up` |  | есть |
| Палитра: выполнить | `palette.run` | `enter` |  | есть |
| Палитра: закрыть | `palette.close` | `escape` |  | есть |
| Новая встреча (помощник планирования) | `calendar.new_meeting` | `ctrl-shift-q` |  | есть |
| Предыдущий период (день, неделя, месяц) | `calendar.prev_week` | `pageup` |  | есть |
| Следующий период (день, неделя, месяц) | `calendar.next_week` | `pagedown` |  | есть |
| Сегодня в календаре | `calendar.today` | `home` |  | есть |
| Вид «День» | `calendar.view_day` | `1` |  | есть |
| Вид «Рабочая неделя» | `calendar.view_work_week` | `2` |  | есть |
| Вид «Неделя» | `calendar.view_week` | `3` |  | есть |
| Вид «Месяц» | `calendar.view_month` | `4` |  | есть |
| Отправить письмо | `compose.send` | `ctrl-enter` |  | есть |
| Вложить файл | `compose.attach` |  |  | есть |
| Закрыть письмо | `compose.close` | `escape` |  | есть |
| Жирный | `compose.bold` | `ctrl-b` |  | есть |
| Курсив | `compose.italic` | `ctrl-i` |  | есть |
| Подчёркнутый | `compose.underline` | `ctrl-u` |  | есть |
| Маркированный список | `compose.bullets` |  |  | есть |
| Нумерованный список | `compose.numbers` |  |  | есть |
| Ссылка | `compose.link` |  |  | есть |
| Цитата | `compose.quote` |  |  | есть |
| Развернуть ответ во вкладку | `compose.expand` |  |  | есть |
| Удалить черновик | `compose.discard` |  |  | есть |
| Отправить приглашения | `meeting.send` | `ctrl-enter` |  | есть |
| Закрыть вкладку встречи | `meeting.close` | `escape` |  | есть |
| Удалить черновик встречи | `meeting.discard` | `ctrl-d` |  | есть |
| Следующее свободное окно | `meeting.next_slot` | `right` |  | есть |
| Предыдущее свободное окно | `meeting.prev_slot` | `left` |  | есть |
| Встреча в окно под курсором | `meeting.pick_slot` | `enter` |  | есть |
| Встреча длиннее на шаг сетки | `meeting.longer` | `shift-right` |  | есть |
| Встреча короче на шаг сетки | `meeting.shorter` | `shift-left` |  | есть |
| Встреча: предыдущий день | `meeting.prev_day` | `pageup` |  | есть |
| Встреча: следующий день | `meeting.next_day` | `pagedown` |  | есть |
| Встреча: сегодня | `meeting.today` | `home` |  | есть |
| Добавить участника из справочника | `meeting.add_person` | `ctrl-shift-b` |  | есть |
| Новый контакт | `contacts.new` | `ctrl-shift-c` |  | есть |
| Изменить контакт | `contacts.edit` | `f2` |  | есть |
| Удалить контакт | `contacts.delete` | `delete` |  | есть |
| Написать контакту | `contacts.write` |  |  | есть |
| Встреча с контактом | `contacts.meeting` |  |  | есть |
| Копировать адрес контакта | `contacts.copy_address` |  |  | есть |
| Меню контакта | `contacts.context_menu` | `shift-f10`, `menu` |  | есть |
| Новая задача | `task.new` | `ctrl-n` |  | есть |
| Выполнить задачу | `task.complete` | `space` |  | есть |
| Срок задачи | `task.due` | `insert` |  | есть |
| Переименовать задачу | `task.edit` | `f2` |  | есть |
| Удалить задачу | `task.delete` | `delete` |  | есть |
| Открыть письмо задачи | `task.open` | `enter` |  | есть |
| Следующая задача | `task.next` | `down` |  | есть |
| Предыдущая задача | `task.prev` | `up` |  | есть |
| Следующее свободное окно | `task.slot_next` | `right` |  | есть |
| Предыдущее свободное окно | `task.slot_prev` | `left` |  | есть |
| Поставить время в календаре | `task.block` |  |  | есть |
| Поручить задачу | `task.delegate` |  |  | есть |
| Напомнить исполнителю | `task.remind` |  |  | есть |
| Напомнить всем, кто не принял поручение | `task.remind_all` |  |  | есть |
| Забрать поручение | `task.revoke` |  |  | есть |
| Принять приглашение | `invite.accept` |  | `a` | есть |
| Ответить «Под вопросом» | `invite.tentative` |  | `t` | есть |
| Отклонить приглашение | `invite.decline` |  | `n` | есть |
| Настройки | `settings.open` | `ctrl-,` |  | есть |
| Автоматические ответы | `settings.vacation` |  |  | есть |
| Правила | `settings.rules` |  |  | есть |
| Настройки календаря | `settings.calendar` |  |  | есть |
| Новая папка | `folder.new` |  |  | есть |
| Переименовать папку | `folder.rename` |  |  | есть |
| Экспорт… | `mail.export` |  |  | есть |
| Экспорт учётки… | `account.export` |  |  | есть |
| Экспорт всего… | `export.all` |  |  | есть |
| Фильтры: папки или фильтры слева | `view.filters` |  |  | есть |
| Быстрые фильтры: показать или скрыть | `view.quick_filters` |  |  | есть |
| Сортировать по дате | `view.sort_by_date` |  |  | есть |
| Сортировать по отправителю | `view.sort_by_sender` |  |  | есть |
| Сортировать по размеру | `view.sort_by_size` |  |  | есть |
| Сбросить фильтры | `filters.reset` |  |  | есть |
