# S8-D — тексты интерфейса (RU + EN)

Утверждаются вместе с макетами (`docs/design/S8-D/*.png`, источники — `src/*.html`). Ключи — предложение для файлов
Fluent из карточек; числа — с формами множественного числа Fluent (`[one]`, `[few]`, `[many]`; в EN — `[one]`,
`*[other]`). `{ $from }`, `{ $to }`, `{ $folder }` — «Учётка · Папка»; `{ $server }` — имя учётки цели.

## Перенос — `migration.ftl` (S8-16)

| Ключ | RU | EN |
|---|---|---|
| migration-ask | { $n } писем перенесены из «{ $from }» | { $n } messages moved from “{ $from }” |
| migration-ask-more | { $n } писем перенесены из «{ $from }» и ещё { $more } папки | { $n } messages moved from “{ $from }” and { $more } more folders |
| migration-ask-exchange | · Метки Postman на Exchange не сохранятся | · Postman labels won’t be kept on Exchange |
| migration-to-server | Перенести на сервер «{ $server }» | Move to server “{ $server }” |
| migration-keep-local | Оставить локальным архивом | Keep as local archive |
| migration-running | Перенос в «{ $to }» | Moving to “{ $to }” |
| migration-running-detail | · { $done } из { $total } · { $bytes } из { $total-bytes } · осталось около { $eta } | · { $done } of { $total } · { $bytes } of { $total-bytes } · about { $eta } left |
| migration-paused | Перенос на паузе | Move paused |
| migration-pause | Пауза | Pause |
| migration-resume | Продолжить | Resume |
| migration-stop | Остановить | Stop |
| migration-no-space | На сервере «{ $server }» нет места: осталось { $n } писем · { $bytes } | No space on “{ $server }”: { $n } messages left · { $bytes } |
| migration-keep-local-short | Оставить локально | Keep locally |
| migration-stop-ask | Остановить перенос: готово { $done } из { $total } | Stop the move: { $done } of { $total } done |
| migration-stop-keep | Оставить остальные локально | Keep the rest locally |
| migration-stop-return | Вернуть на место | Move back |
| migration-stop-no | Не останавливать | Keep going |
| migration-status | Перенос: { $done } из { $total } · { $bytes } | Moving: { $done } of { $total } · { $bytes } |
| migration-status-paused | Перенос на паузе: { $done } из { $total } | Move paused: { $done } of { $total } |
| migration-status-no-space | Перенос стоит: нет места | Move stopped: no space |
| migration-done | Перенесено на сервер «{ $server }»: { $n } писем | Moved to “{ $server }”: { $n } messages |
| migration-done-source | У «{ $source }» письма удалены после ответа «{ $server }» | Deleted from “{ $source }” after “{ $server }” confirmed |
| migration-done-partial | Перенесено на «{ $server }»: { $done } из { $total } | Moved to “{ $server }”: { $done } of { $total } |
| migration-too-large | { $n } письма больше предела сервера ({ $limit }) — остались на этом компьютере | { $n } messages exceed the server limit ({ $limit }) and stay on this computer |
| migration-show | Показать | Show |
| migration-done-local | { $n } писем — только на этом компьютере | { $n } messages are on this computer only |
| migration-done-local-detail | «{ $to }», у «{ $source }» удалены | “{ $to }”, deleted from “{ $source }” |
| migration-returned | { $n } писем возвращены в «{ $from }» | { $n } messages moved back to “{ $from }” |
| migration-row-pending | Ждёт переноса на сервер «{ $server }» | Waiting to move to “{ $server }” |
| migration-row-paused | Перенос на паузе | Move paused |
| migration-row-no-space | Ждёт места на сервере «{ $server }» | Waiting for space on “{ $server }” |
| migration-row-local | Только на этом компьютере — на сервере «{ $server }» его нет | On this computer only — not on “{ $server }” |
| migration-row-too-large | Только на этом компьютере: { $size } — больше предела «{ $server }» ({ $limit }) | On this computer only: { $size } exceeds the “{ $server }” limit ({ $limit }) |

## «На этом компьютере» — `local.ftl` (S8-15)

| Ключ | RU | EN |
|---|---|---|
| local-group | На этом компьютере | On this computer |
| local-group-hint | Хранится только в базе Postman на этом компьютере · { $n } писем · { $bytes } | Stored only in Postman’s database on this computer · { $n } messages · { $bytes } |
| local-new-folder | Новая папка | New folder |
| local-rename | Переименовать | Rename |
| local-delete | Удалить | Delete |
| local-delete-not-empty | Удалить можно пустую папку | Only an empty folder can be deleted |
| local-drop | Перенести { $n } письма сюда | Move { $n } messages here |

## Экспорт — `export.ftl`, `commands.ftl` (S8-32)

| Ключ | RU | EN |
|---|---|---|
| cmd-mail-export, cmd-folder-export | Экспорт… | Export… |
| cmd-account-export | Экспорт учётки… | Export account… |
| cmd-export-all | Экспорт всего… | Export everything… |
| export-what-folder | Экспорт «{ $folder }» · { $n } писем | Export “{ $folder }” · { $n } messages |
| export-what-selection | Экспорт: { $n } письма | Export: { $n } messages |
| export-what-account | Экспорт учётки «{ $account }» · { $n } писем | Export account “{ $account }” · { $n } messages |
| export-what-all | Экспорт всего · { $n } писем | Export everything · { $n } messages |
| export-subfolders | С подпапками ({ $n }) | Include subfolders ({ $n }) |
| export-start | Выбрать папку и начать… | Choose folder and start… |
| export-cancel | Отмена | Cancel |
| export-eml | Файл на письмо · Outlook, Thunderbird, Apple Mail | One file per message · Outlook, Thunderbird, Apple Mail |
| export-mbox | Файл на папку · Thunderbird, Apple Mail | One file per folder · Thunderbird, Apple Mail |
| export-maildir | Каталог на папку · mutt, Dovecot | One directory per folder · mutt, Dovecot |
| export-msg | Файл на письмо · Outlook: перетащить в папку | One file per message · Outlook: drag into a folder |
| export-pst | Файл данных Outlook · «Импорт и экспорт» | Outlook data file · “Import and Export” |
| export-running | Экспорт в { $format } | Exporting to { $format } |
| export-running-detail | · { $done } из { $total } · загрузка с сервера: { $fetch } | · { $done } of { $total } · downloading from server: { $fetch } |
| export-status | Экспорт: { $done } из { $total } | Export: { $done } of { $total } |
| export-stop | Остановить | Stop |
| export-done | Экспортировано { $n } писем | Exported { $n } messages |
| export-done-partial | Экспортировано { $done } из { $total } | Exported { $done } of { $total } |
| export-failed-fetch | { $n } писем не загрузились с сервера «{ $server }» | { $n } messages couldn’t be downloaded from “{ $server }” |
| export-stopped | Экспорт остановлен: записано { $done } из { $total } | Export stopped: { $done } of { $total } written |
| export-stopped-detail | Готовые файлы целы, недописанный удалён | Finished files are intact, the unfinished one was removed |
| export-open | Открыть папку | Open folder |
| export-retry | Повторить | Retry |

## Просмотр — `attachments.ftl` (S8-23, S8-27)

| Ключ | RU | EN |
|---|---|---|
| att-pv-sheet | Лист «{ $name }» | Sheet “{ $name }” |
| att-pv-slide | Слайд { $n } | Slide { $n } |
| att-pv-figure | [рисунок] | [picture] |
| att-pv-notes | Заметки докладчика | Speaker notes |
| att-pv-cut-rows | Показано начало: { $shown } из { $total } строк | Showing the first { $shown } of { $total } rows |
| att-pv-letter-from / -to / -cc / -date | От / Кому / Копия / Дата | From / To / Cc / Date |
| att-pv-module | Просмотр: модуль «{ $name }» { $version } | Viewer: module “{ $name }” { $version } |
| att-pv-module-third | Просмотр: модуль «{ $name }» { $version } (сторонний) · в песочнице | Viewer: module “{ $name }” { $version } (third-party) · sandboxed |
| att-pv-module-enable | Включить модуль «{ $name }» | Turn on module “{ $name }” |

## Настройки — `settings.ftl` (S8-27, S8-16)

| Ключ | RU | EN |
|---|---|---|
| settings-nav-viewers | Просмотр вложений | Attachment viewers |
| settings-hint-viewers | Файлы открываются в песочнице, без внешних программ. Новые форматы — модулями. | Files open in a sandbox, without other apps. New formats come as modules. |
| settings-viewers-builtin | Встроенные | Built-in |
| settings-viewers-docs / -sheets / -slides / -images / -mail / -archives | Документы / Таблицы / Презентации / Картинки / Письма / Архивы | Documents / Spreadsheets / Presentations / Images / Messages / Archives |
| settings-viewers-zip | ZIP — оглавление, без распаковки | ZIP — contents list, not extracted |
| settings-viewers-modules | Модули | Modules |
| settings-viewers-third | сторонний | third-party |
| settings-viewers-admin | от администратора | from administrator |
| settings-viewers-broken-imports | Не загружается: модуль просит доступ к системе — такие не запускаются | Not loaded: the module asks for system access, which isn’t allowed |
| settings-viewers-none | Модулей нет. Положите файл .wasm в папку модулей. | No modules. Put a .wasm file into the modules folder. |
| settings-viewers-open | Открыть папку модулей | Open modules folder |
| settings-viewers-tip | Модуль — файл .wasm с описанием .json. Сторонние — без подписи; видят только открытый файл. | A module is a .wasm file with a .json description. Third-party modules are unsigned and see only the opened file. |
| settings-bitlocker-off | Диск { $drive } с данными Postman не зашифрован BitLocker | Drive { $drive } with Postman data isn’t encrypted with BitLocker |
| settings-bitlocker-off-hint | Письма «На этом компьютере» хранятся только здесь. Включите BitLocker или обратитесь к администратору. | Messages “On this computer” are stored only here. Turn on BitLocker or contact your administrator. |
| settings-bitlocker-open | Параметры BitLocker | BitLocker settings |
| settings-bitlocker-on | Диск { $drive } с данными Postman зашифрован BitLocker | Drive { $drive } with Postman data is encrypted with BitLocker |
| settings-bitlocker-unknown | Шифрование диска { $drive } неизвестно | Encryption of drive { $drive } is unknown |
| settings-bitlocker-unknown-hint | Windows не сообщила состояние BitLocker без прав администратора. | Windows didn’t report BitLocker status without administrator rights. |

## Хранилище — `sync.ftl` (S8-16; `sync-store-upgrade` уже есть)

| Ключ | RU | EN |
|---|---|---|
| sync-store-upgrade | Обновление хранилища: { $percent } % | Updating storage: { $percent }% |
| sync-store-upgrade-hint | Один раз после обновления Postman. Письма на сервере не меняются. | Once after a Postman update. Messages on the server don’t change. |
| sync-store-upgrade-failed | Хранилище не обновилось — письма загружаются с сервера заново | Storage update failed — messages are being downloaded from the server again |
| sync-store-upgrade-failed-detail | Прежние файлы не тронуты. Причина — в журнале. | Previous files are untouched. The reason is in the log. |
