# Задачи (S6-05, Ctrl+4): списки, проекты, категории, список с группами, карточка задачи.
tasks-inbox = Входящие
tasks-today = Сегодня
tasks-upcoming = Предстоящие
tasks-from-mail = Из почты
tasks-someday = Когда-нибудь
tasks-done-list = Выполненные
tasks-projects = Проекты
tasks-categories = Категории
tasks-nav-note = Письмо со «Сроком» само становится задачей. Выполнили задачу — срок у письма снимается.
tasks-search = Поиск в задачах
tasks-quick-placeholder = Добавить задачу
tasks-new = Новая задача
tasks-complete = Выполнить
tasks-reopen = Вернуть в работу
tasks-edit = Изменить
tasks-delete = Удалить
tasks-pick = Выберите задачу слева.
tasks-count = { $n ->
    [one] { $n } задача
    [few] { $n } задачи
   *[many] { $n } задач
}
tasks-summary = { tasks-count }
tasks-summary-today = { $day } · { tasks-count }
tasks-summary-today-over = { $day } · { tasks-count } · { $over } просрочено
tasks-summary-mail = письма со сроком · { tasks-count }
tasks-summary-project = выполнено { $done } из { $total }
tasks-group-overdue = Просрочено
tasks-group-today = Сегодня
tasks-group-timed = ко времени
tasks-group-no-time = Без времени
tasks-group-tomorrow = Завтра
tasks-group-soon = Скоро
tasks-group-later = Позже
tasks-group-no-due = Без срока
tasks-group-yesterday = Вчера
tasks-group-earlier = Ранее
tasks-empty-inbox = Входящие пусты. Сюда попадают задачи без срока и проекта — разберите их, когда будет минута.
tasks-empty-today = На сегодня всё. Хорошего вечера.
tasks-empty-upcoming = Впереди пусто.
tasks-empty-mail = Писем со сроком нет. Поставьте «Срок» письму — оно появится здесь.
tasks-empty-someday = Здесь будут идеи и задачи без срока.
tasks-empty-done = Пока ничего не выполнено.
tasks-empty = Здесь задач нет.
tasks-kind-task = Задача
tasks-kind-mail = Из почты
tasks-letter = Письмо
tasks-open = Открыть
tasks-letter-gone = Письма больше нет в ящике.
tasks-field-due = Срок
tasks-field-remind = Напоминание
tasks-field-repeat = Повтор
tasks-field-project = Проект
tasks-field-category = Категория
tasks-due-at = { $day }, до { $time }
tasks-no-due = Без срока
tasks-no-remind = Нет
tasks-remind-set = { $n ->
    [one] { $n } напоминание
    [few] { $n } напоминания
   *[many] { $n } напоминаний
}
tasks-repeats = Повторяется
tasks-no-repeat = Не повторяется
tasks-no-project = Без проекта
tasks-no-category = Без категории
tasks-progress = { $done } из { $total }
tasks-subtasks = Подзадачи
tasks-no-subtasks = Подзадач нет
tasks-notes = Заметки
tasks-no-notes = Заметок нет
tasks-done = Выполнено · «{ $title }»
tasks-done-mail = Выполнено · срок у письма снят
tasks-reopened = Задача снова в работе
tasks-deleted = Задача удалена
tasks-deleted-mail = Удалено · срок у письма снят
tasks-status-mail = { $n ->
    [one] { $n } задача из писем
    [few] { $n } задачи из писем
   *[many] { $n } задач из писем
}
tasks-failed = Задача не сохранена на сервере: { $error }
tasks-project-new = Новый проект — Enter
tasks-sub-add = Добавить подзадачу
tasks-notes-placeholder = Добавить заметку…
tasks-important = важно
tasks-remind-at-due = В срок
tasks-remind-minutes = За { $n } мин
tasks-remind-hours = За { $n } ч
tasks-remind-days = За { $n } д
tasks-remind-at = В { $when }
tasks-block = Время в календаре
tasks-block-remove = Убрать
tasks-block-none = Сегодня свободного окна нет
tasks-block-placed = В календаре: { $when }
tasks-done-next = Выполнено · следующая — { $when }
tasks-repeat-daily = Каждый день
tasks-repeat-days = Каждые { $n } дн.
tasks-repeat-weekdays = По будням
tasks-repeat-weekly = Каждую неделю
tasks-repeat-weeks = Каждые { $n } нед.
tasks-repeat-monthly = Каждый месяц
tasks-repeat-months = Каждые { $n } мес.
tasks-repeat-monthly-day = Каждый месяц, { $day }-го
tasks-repeat-yearly = Каждый год
tasks-repeat-after = Через { $n } д после выполнения
tasks-repeat-other = Повторяется (особое правило)
tasks-snooze = Отложить на 10 мин
tasks-remind-title = Напоминание о задаче
tasks-remind-toast = Напоминание: «{ $title }»


# Поручения (S6-10): форма «Поручить», карточка поручения, письма шагов.
tasks-delegate = Поручить
tasks-remind = Напомнить
tasks-revoke = Забрать
assign-you = Вы
assign-chip-mine = Поручено вами
assign-chip-theirs = Поручено вам
assign-chip-watch = Вы наблюдаете
assign-watch-owner = Поручитель: { $name }
assign-watch-owner-in = Поручитель: { $name } · { $project }
assign-role-assignee = Исполнитель
assign-role-owner = Автор поручения
assign-st-waiting = Ждёт принятия
assign-st-proposed = Предложен новый срок
assign-st-accepted = Принято
assign-st-in-progress = В работе
assign-st-done = Готово — на проверку
assign-st-declined = Отклонено
assign-st-closed = Принято вами
assign-st-revoked = Забрано
assign-me-st-waiting = Ждёт вашего ответа
assign-me-st-proposed = Вы предложили срок
assign-me-st-accepted = Вы приняли
assign-me-st-in-progress = В работе
assign-me-st-done = На проверке у автора
assign-me-st-declined = Вы отклонили
assign-me-st-closed = Результат принят
assign-me-st-revoked = Поручение отозвано
assign-rib-1 = Поручено
assign-rib-2 = Принято
assign-rib-3 = В работе
assign-rib-4 = Готово
assign-rib-5 = Принято вами
assign-rib-declined = Отклонено
assign-rib-checked = Проверено
assign-field-due = Срок
assign-field-proposed = Предложенный срок
assign-field-watchers = Наблюдатели
assign-none = Нет
assign-history = История
assign-step = { $who } — { $what }
assign-what-assigned = поручено
assign-what-accepted = принято
assign-what-declined = отклонено
assign-what-proposed = предложен срок: { $due }
assign-what-started = в работе
assign-what-finished = готово, на проверку
assign-what-recalled = отозвано с проверки
assign-what-reopened = ответ изменён
assign-what-reminded = напоминание
assign-what-due-changed = новый срок: { $due }
assign-what-agreed = срок согласован: { $due }
assign-what-kept = срок оставлен прежним
assign-what-result-accepted = результат принят, задача закрыта
assign-what-returned = возвращено на доработку
assign-what-revoked = поручение забрано
assign-what-reassigned = поручено другому исполнителю
assign-note-result = Результат
assign-note-proposed = Предлагает срок
assign-note-declined = Причина отказа
assign-note-returned = Что доработать
assign-note-assigned = Комментарий к поручению
assign-accept-result = Принять результат
assign-return = Вернуть с комментарием
assign-do-return = Вернуть
assign-change-due = Изменить срок
assign-revoke = Забрать задачу
assign-agree = Согласиться: { $due }
assign-keep = Оставить срок
assign-reassign = Поручить другому
assign-remind = Напомнить
assign-reminded = Напомнили { $time }
assign-accept = Принять
assign-propose = Предложить срок
assign-decline = Отклонить
assign-start = В работе
assign-finish = Готово — отправить на проверку
assign-recall = Отозвать с проверки
assign-reopen = Изменить ответ
assign-send = Отправить
assign-cancel = Отмена
assign-delegate = Поручить
assign-new-due = Новый срок
assign-due-note = { $who } получит новый срок в задаче
assign-pick-due = Выберите срок
assign-comment-return = Что доработать — увидит { $who }
assign-comment-decline = Причина — увидит { $who }
assign-comment-propose = Комментарий — увидит { $who }
assign-sent-review = Отправлено на проверку
assign-proposed-due = Вы предложили срок: { $due }
assign-closed = Результат принят — задача закрыта
assign-closed-theirs = Результат принят автором — задача закрыта
assign-revoked = Поручение отозвано автором
assign-revoked-mine = Задача забрана — снова ваша
assign-due-today = Сегодня
assign-due-tomorrow = Завтра
assign-due-text = { $day }, до { $time }
assign-form-title = Поручить
assign-form-person = Исполнитель
assign-form-person-placeholder = Имя, отдел или адрес
assign-form-person-hint = Справочник и ваши контакты · раскладка не важна
assign-form-watchers = Наблюдатели
assign-form-watchers-opt = · необязательно
assign-form-watcher-placeholder = Добавить — видят ход работы, не исполняют
assign-form-due = Срок
assign-form-comment = Комментарий
assign-form-comment-placeholder = Что важно — увидит исполнитель
assign-form-note = { $who } увидит задачу в «Мне поручили»
assign-form-note-empty = Исполнитель — один
assign-form-pick-first = Сначала выберите исполнителя
assign-invalid = Сейчас это действие недоступно
assign-too-soon = Уже напомнили — следующее напоминание не раньше { $time }
assign-not-yet = Поручения ещё нет в задачах — оно появится через несколько секунд
assign-failed = Письмо поручения не ушло: { $error }
assign-toast-saved = Поручение сохранено
assign-toast-demo = { $text } · демо: письмо не отправлено
assign-toast-assigned = Поручено: «{ $title }» — ждёт принятия
assign-toast-accepted = Вы приняли «{ $title }»
assign-toast-declined = Вы отклонили «{ $title }»
assign-toast-proposed = Предложен новый срок: «{ $title }»
assign-toast-started = «{ $title }» — в работе
assign-toast-finished = «{ $title }» — отправлено на проверку
assign-toast-recalled = «{ $title }» — отозвано с проверки
assign-toast-reopened = Ответ по «{ $title }» изменён
assign-toast-reminded = Напомнили по «{ $title }»
assign-toast-due-changed = Новый срок «{ $title }» — исполнитель увидит
assign-toast-agreed = Срок согласован: «{ $title }»
assign-toast-kept = Срок «{ $title }» оставлен прежним
assign-toast-result-accepted = Результат принят: «{ $title }» закрыта
assign-toast-returned = «{ $title }» — возвращено на доработку
assign-toast-revoked = «{ $title }» — задача снова ваша
assign-toast-reassigned = «{ $title }» — поручено другому
assign-mail-card-request = Поручение
assign-mail-card-reply = Ответ по поручению
assign-mail-card-counter = Предложен новый срок
assign-mail-card-cancel = Поручение отозвано
assign-mail-card-declinecounter = Срок оставлен прежним
assign-mail-from = От: { $who }
assign-open-tasks = Открыть в задачах
assign-mail-due = Срок: { $due }
assign-mail-comment = Комментарий: { $text }
assign-mail-footer = Если у вас нет Postman, ответьте на это письмо — ответ увидит { $who }.
assign-mail-subject-assigned = Поручение: { $title }
assign-mail-subject-accepted = Принято: { $title }
assign-mail-subject-declined = Отклонено: { $title }
assign-mail-subject-proposed = Предложен срок: { $title }
assign-mail-subject-started = В работе: { $title }
assign-mail-subject-finished = Готово, на проверку: { $title }
assign-mail-subject-recalled = Отозвано с проверки: { $title }
assign-mail-subject-reopened = Ответ изменён: { $title }
assign-mail-subject-reminded = Напоминание: { $title }
assign-mail-subject-due-changed = Новый срок: { $title }
assign-mail-subject-agreed = Срок согласован: { $title }
assign-mail-subject-kept = Срок прежний: { $title }
assign-mail-subject-result-accepted = Результат принят: { $title }
assign-mail-subject-returned = На доработку: { $title }
assign-mail-subject-revoked = Поручение отозвано: { $title }
assign-mail-subject-reassigned = Поручение: { $title }
assign-mail-body-assigned = { $who } поручает вам задачу «{ $title }».
assign-mail-body-accepted = { $who } принимает поручение «{ $title }».
assign-mail-body-declined = { $who } отклоняет поручение «{ $title }».
assign-mail-body-proposed = { $who } предлагает новый срок для «{ $title }».
assign-mail-body-started = { $who } берёт «{ $title }» в работу.
assign-mail-body-finished = { $who }: «{ $title }» готово и отправлено вам на проверку.
assign-mail-body-recalled = { $who } отзывает «{ $title }» с проверки — снова в работе.
assign-mail-body-reopened = { $who } меняет ответ по «{ $title }».
assign-mail-body-reminded = { $who } напоминает о задаче «{ $title }».
assign-mail-body-due-changed = { $who } меняет срок задачи «{ $title }».
assign-mail-body-agreed = { $who } соглашается с новым сроком задачи «{ $title }».
assign-mail-body-kept = { $who } оставляет прежний срок задачи «{ $title }».
assign-mail-body-result-accepted = { $who } принимает результат: «{ $title }» закрыта. Спасибо!
assign-mail-body-returned = { $who } возвращает «{ $title }» на доработку.
assign-mail-body-revoked = { $who } забирает задачу «{ $title }» — делать её больше не нужно.
assign-mail-body-reassigned = { $who } поручает вам задачу «{ $title }».
# Снимок `delegate`: комментарий формы (демо).
assign-screen-comment = Собрать вопросы заказчику: сроки поставки, требования, лицензии и SLA.
assign-screen-title = Собрать требования к стенду для пилота

# Раздел «Поручения» (S6-11, макеты «Поручил я», «Поручено мне»).
tasks-delegations = Поручения
deleg-count = { $n ->
    [one] { $n } поручение
    [few] { $n } поручения
   *[many] { $n } поручений
}
deleg-summary-by-me = { $review ->
    [0] { deleg-count }
   *[other] { deleg-count } · { $review } на проверку
}
deleg-summary-to-me = { $new ->
    [0] { deleg-count }
   *[other] { deleg-count } · { $new } ждут ответа
}
deleg-tab-by-me = Я поручил
deleg-tab-to-me = Мне поручили
deleg-own-only = Только свои
deleg-group-review = На проверку
deleg-group-review-sub = ждут вашего решения
deleg-group-work = В работе
deleg-group-waiting = Ждут принятия
deleg-group-new = Новые
deleg-group-new-sub = ждут вашего ответа
deleg-group-checking = На проверке
deleg-group-checking-sub = у автора
deleg-remind-all = Напомнить всем
deleg-watching = наблюдаю
deleg-last = { $when } — { $what }
deleg-last-watched = поручил(а) { $who } · { $last }
deleg-proposes = Предлагает: { $due }
deleg-wait-hint = { $n ->
    [one] Невыполненное поручение ({ $n }) само попадает в почту — в быстрый фильтр «Ждём»
   *[other] Невыполненные поручения ({ $n }) сами попадают в почту — в быстрый фильтр «Ждём»
}
deleg-open-wait = Открыть в почте
deleg-status-waiting = В «Ждём»: { deleg-count }
deleg-empty-by-me = Поручений нет. Откройте задачу и нажмите «Поручить».
deleg-empty-to-me = Вам ничего не поручили.
assign-toast-taken = Задача снова ваша · в «{ $list }»
assign-toast-deleted = Поручение удалено · { $who } увидит
assign-delete-not-mine = Поручение не ваше: его удаляет автор. Можно отклонить.
assign-withhold-late = Письмо уже ушло — исполнитель получил отмену
assign-toast-reminded-all = { $n ->
    [one] Напомнили: { $n } исполнителю
   *[other] Напомнили: { $n } исполнителям
}{ $soon ->
    [0] {""}
   *[other] {" "}· { $soon } — уже сегодня
}
assign-remind-all-soon = Всем уже напомнили сегодня — следующее напоминание через сутки

# Сверка макетов T2-G (T2-30…T2-36): строка задачи, порядок, вид, проекты, история, тосты, исполнитель, поручения.
tasks-open-letter-tip = Открыть письмо «{ $subject }»
tasks-no-due-project = Без срока и проекта
tasks-sort-due = По сроку
tasks-sort-manual = Вручную
tasks-sort-done = По дате выполнения
tasks-sort-status = По статусу
tasks-sort-title = По названию
tasks-sort-important = Сначала важные
tasks-move-today = Перенести на сегодня
tasks-moved-today = { $n ->
    [one] Перенесено на сегодня: { $n } задача
    [few] Перенесено на сегодня: { $n } задачи
   *[many] Перенесено на сегодня: { $n } задач
}
tasks-project-add = Новый проект
tasks-added = Добавлено · { $when }
tasks-added-where = в «{ $list }»
tasks-added-at = { $day }, { $time }
tasks-show = Показать
tasks-day-today = сегодня
tasks-day-tomorrow = завтра
tasks-day-yesterday = вчера
tasks-plan = Запланировать
tasks-to-project = В проект
tasks-reply = Ответить автору
tasks-reply-subject = Re: { $title }
tasks-view = Вид списка
tasks-view-list = Вид: список
tasks-view-dense = Вид: плотный
tasks-view-item-list = Список
tasks-view-item-dense = Плотный список — в одну строку
tasks-history = История
tasks-history-n = История · { $n }
tasks-hist-created = Создана · { $when }
tasks-hist-from-mail = Создана из письма «{ $subject }» · { $when }
tasks-hist-from-mail-plain = Создана из письма «{ $subject }»
tasks-hist-from-mail-unknown = Создана из письма
tasks-hist-due-moved = Срок перенесён с { $from } на { $to } · { $when }
tasks-hist-due-set = Срок: { $to } · { $when }
tasks-hist-due-removed = Срок снят · { $when }
tasks-hist-done = Выполнена · { $when }
tasks-hist-reopened = Снова в работе · { $when }
tasks-hist-subtasks = Подзадачи: { $done } из { $total } · { $when }
assign-form-title-new = Новое поручение
assign-form-title-placeholder = Что поручить — название задачи
assign-form-title-first = Сначала напишите, что поручить
assign-load = { $n } в работе
assign-load-none = нет поручений
assign-hint-return = Статус станет «В работе»
assign-hint-propose = Пока автор не решит, срок прежний
assign-hint-decline = Задача уйдёт из ваших списков
deleg-group-declined = Отклонённые
deleg-status-new = { $n ->
    [one] { $n } новое поручение
    [few] { $n } новых поручения
   *[many] { $n } новых поручений
}
deleg-status-no-new = Новых поручений нет
