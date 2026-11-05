# Стенд Stalwart и поведение сервера

Чего сервер не умеет или делает неверно — сводным списком для отчётов и настройки: `docs/STALWART-GAPS.md`.

Что проверено на живом сервере и как клиент это учитывает. Стенд: Stalwart v0.16.27 (Docker на ВМ),
домен `itt.test`, пользователи `user1…user3`; доступ, пароли и эталонная коллекция — у владельца
(`2026 ITT Infra\stalwart`, вне репозитория). Внутренние адреса и пароли сюда не пишем.

## Запуск проверок

Доступ (с 2026-10-20): из офисной сети напрямую — только HTTPS 443 (`https://<адрес стенда>`), порт 8080 закрыт;
через туннель владельца (`stalwart-tunnel.ps1`) — `http://localhost:18080`. У стенда сертификат по умолчанию
(rcgen, `CN=rcgen self signed cert`, SAN только `localhost`): ни IP, ни `mail.itt.test` он не покрывает, в доверенные
его не добавить. Поэтому напрямую — с `POSTMAN_INSECURE_TLS=1` (проверка сертификата выключена, в журнале
предупреждение; только стенд). Нужный выход — сертификат сервера на его имя от внутреннего CA (задача стенда).
Прокси: если задан `HTTPS_PROXY`, в `NO_PROXY` нужен `10.0.0.0/8`, иначе reqwest пойдёт к стенду через прокси.

```
POSTMAN_STAND_URL=https://<адрес стенда> POSTMAN_INSECURE_TLS=1   # или http://localhost:18080 через туннель
POSTMAN_STAND_USER1=user1@itt.test POSTMAN_STAND_PASSWORD1=…
POSTMAN_STAND_USER2=user2@itt.test POSTMAN_STAND_PASSWORD2=…
POSTMAN_STAND_USER3=user3@itt.test POSTMAN_STAND_PASSWORD3=…
cargo test -p postman-backend-jmap --test stand -- --ignored
```

Пароли — только в окружении процесса. Тесты (`crates/backend-jmap/tests/stand.rs`, IMAP и «стандарты» — `crates/backend-imap/tests/`) идут тем же кодом, что и приложение,
и за собой убирают (письма, события, карточки); в CI не запускаются (`#[ignore]`).

Клиент CalDAV и CardDAV (S7-05а, `crates/dav/tests/stand.rs`) — тем же HTTPS 443 (`POSTMAN_STAND_DAV`, иначе
`POSTMAN_STAND_URL`), user1 и user2; письма сервера о встрече убираются по IMAP, если задан `POSTMAN_STAND_IMAP`
(`<адрес стенда>:993`):

```
cargo test -p postman-dav --test stand -- --ignored --test-threads=1
```

| Тест | Что проверяет |
|---|---|
| `session_follows_connection_origin_and_has_capabilities` | адреса сессии — на адрес подключения; capabilities |
| `mail_is_sent_received_searched_and_flagged` | отправка (blob → черновик → EmailSubmission), поиск у получателя, тело, `$seen`, `Email/changes` |
| `push_delivers_state_change` | EventSource: событие `state` после нового письма |
| `invitation_round_trip` | приглашение → у участника по uid → «принять» → у организатора accepted → удаление → отмена |
| `free_busy_after_sharing` | `Calendar/set shareWith` → `Principal/getAvailability` у коллеги |
| `contacts_are_read` | `ContactCard` → имя и адреса |
| `contacts_are_edited_and_directory_is_listed` | S4-02, user3: `ContactCard/set` create (телефон, организация, должность) → update патчем (адрес добавлен, телефон убран `null`, имя и должность другие) → destroy; `Principal/query` без фильтра — user1…3 типа `individual` |
| `contacts_through_sync` | S4-02, user3, через поток синхронизации: после входа `ContactsLoaded` и `DirectoryLoaded`; `SaveContact` (новая, затем правка) и `DeleteContact` → итог и свежий `ContactsLoaded` |
| `calendar_through_sync` | S3-05, через поток синхронизации: `ShareFreeBusy` (user3 → user1), `CreateMeeting` с приглашением → у user3 по uid, `Events` → встреча в неделе, `Availability` → занятость user3 |
| `event_keywords_sequence_and_principal_kinds` | S5-11: событие с `keywords` и `categories` → оба возвращаются как есть; встреча с user3 → перенос с рассылкой → `sequence` 0 → 1 и у организатора, и у участника; справочник (`Principal/query` без фильтра) — типы на стенде |
| `rejected_password_stops_sync_with_auth_error` | S4-03: неверный пароль → `Error` рода `Auth` на шаге `Connect`, поток синхронизации закрывается без повторов |
| `load_more_through_sync` | S4-03: `LoadMore` после первой страницы → `LoadedMore` с числом писем следующей страницы |
| `calendar_change_arrives_by_push` | S4-03: событие календаря, созданное другим клиентом → push → один `CalendarChanged` (~1,5 с, задержка на пачку), письма не перечитываются |
| `stand_settings::vacation_reply_round_trip` | S4-05: автоответ user2 включён (`settings::Request::SetVacation`) → письмо от user1 → автоответ у user1 → выключен, письма удалены |
| `stand_settings::quota_is_read` | S4-05: `Quota/get` без ошибки (на стенде квот нет — пустой список) |
| `stand_settings::rule_files_by_subject_with_vacation` | S4-07: папка Y (`Mailbox/set`), правило «тема содержит X → Y» (`SaveRules`), автоответ при правилах → письмо от user1 в Y у user2 и автоответ у user1 → правила, папка, письма убраны |
| `stand_settings::invalid_script_is_rejected_by_validate` | S4-07: неверный скрипт — `SaveError::Invalid` с местом ошибки, скрипты сервера не изменились |
| `stand_shared::shared_inbox_is_synced_read_and_flagged` | S4-06, user3: ящик группы `shared-sales` в сессии и в кэше (ключи `<аккаунт>/<id>`); письмо на адрес группы → по push аккаунта группы в кэш (~1 с) → `Body` → `Keyword $flagged` на сервере в аккаунте группы → убрано |
| `stand_shared::mail_is_sent_as_the_shared_mailbox` | S4-06: `Senders` — свой адрес первым, адрес группы — из её аккаунта; `Send` от имени группы → у получателя «От» группы, копия — в «Отправленных» группы, не в своих → убрано |
| `stand_shared::read_only_share_has_rights_but_no_identity` | S4-06: папка `shared-ro`, открытая только для чтения: `myRights` без добавления, удаления и отправки; `Identity/get` — `forbidden` |
| `stand_labels::categories_are_set_queried_and_cleared` | S5-02, user3: письмо создаётся во «Входящих» (`Email/set create`, без SMTP) → категории патчем `keywords/<слово>` (в том числе слово с `~` и `/`) → `hasKeyword` в папке и во всех папках, `notKeyword`, счётчики `limit: 0` → снятие одной категории не трогает другие → неизвестный id — `notUpdated` (`notFound`) → письмо удалено |
| `stand_labels::categories_through_sync` | S5-02, user3, через поток синхронизации: кэш поменян, `SetKeywords` доводит метку до сервера, `Query` → `SearchResults`, `Counts` → `Counts`; снятие — и в кэше, и на сервере |
| `stand_labels::category_table_fits_in_a_file_node` | S5-02: таблица категорий (JSON) — blob → `FileNode/set create` → `FileNode/get` и чтение blob → `destroy` |
| `stand_categories::table_is_shared_between_sessions_last_writer_wins` | S5-02б, user3: таблицы нет → первая запись создаёт `.postman/categories.json` → вторая сессия читает то же → правки двух клиентов последовательно и одновременно (`tokio::join!`) сводятся по категориям (поздняя побеждает, удаление и новая на месте) → запись с прежним `ifInState` — `stateMismatch`, создание существующего — `alreadyExists` → как было |
| `stand_categories::tables_through_sync_and_shared_mailbox` | S5-02б, через поток синхронизации: при входе `Loaded` своей таблицы и таблицы `shared-sales` (своя у ящика), `Unsupported` у папки `shared-ro` → `Save` командой → `Saved` → запись другого клиента приходит push (`FileNode`) → `Loaded` с ней (29 мс – 1 с) → `Save` в аккаунт ящика не трогает свою → как было |
| `stand_categories::purge_removes_the_keyword_from_messages` | S5-02б: удаление категории «и с писем» — `Purge` снимает слово с двух писем во всех папках, `$seen` остаётся → письма удалены |
| `stand_send_later::held_submission_is_canceled_and_not_delivered` | S5-06: user1 → user3 с `HOLDFOR` 30 → `pending`, `sendAt` через 30 с, в списке ждущих → `canceled` → письмо в «Черновиках» с `$draft`, повторная отмена — `cannotUnsend` → после срока у user3 ничего → убрано |
| `stand_send_later::held_submission_is_delivered_after_the_delay` | S5-06: `HOLDUNTIL` через 20 с → `sendAt` тот же; через 8 с у user3 ничего, затем письмо приходит, `receivedAt` ≥ `sendAt`; отмена после выпуска — `cannotUnsend` → убрано |
| `stand_send_later::send_later_through_sync` | S5-06, через поток синхронизации: `Scheduled` при входе (`max_delay` 7 дней) → `Send` с `Release::At` (+2 мин) → `Sent` со сроком → `Scheduled` с ним → `Unsend` → `Unsent`, в `Scheduled` его нет → убрано |
| `stand_threads::reply_chain_is_one_thread_with_new_text_only` | S5-05: user1 → user3 → user1 цепочкой ответов (In-Reply-To, цитата Postman) → у user3 `Command::Thread` (`Thread/get` + `Email/get`) — одна беседа из трёх писем, своё — из «Отправленных», по времени; «только новый текст» — в каждом письме только его строки → письма убраны |
| `stand_tasks::tasks_capability_and_vtodo_in_calendar` | S6-01: `urn:ietf:params:jmap:tasks` не объявлен; у JMAP `Calendar` нет признака компонентов; календарь по умолчанию в CalDAV — VEVENT и VTODO (п. 37) |
| `stand_tasks::jmap_task_fields_round_trip` | S6-01: `CalendarEvent` `@type: Task` со всеми полями карточки → `get` как есть → тот же VTODO в CalDAV (`X-POSTMAN-*` из `iCalendar.properties`) → выполнение патчем → `CalendarEvent/changes` у второго клиента (создание, правка, удаление); `recurrenceRules`, `progressUpdated`, свойство поставщика — `invalidProperties` (п. 38) |
| `stand_tasks::task_query_by_due_and_status` | S6-01: три задачи → `after`/`before` по сроку, `title`; условий `progress`/`status`/`type` нет (`unsupportedFilter`), сортировки `due` — тоже (`unsupportedSort`); CalDAV `time-range` и `prop-filter STATUS` (и `negate-condition`) — на сервере (п. 39) |
| `stand_tasks::caldav_vtodo_round_trip_and_sync` | S6-01: `PUT` VTODO с полями карточки → `GET` как есть → вид в JMAP (CATEGORIES → `keywords`, ATTACH → `links`, X- и COMMENT → `iCalendar`) → выполнение с `If-Match`, старый ETag — 412 → `sync-collection`: правка и удаление (п. 38, 39) |
| `stand_tasks::filenode_task_list_round_trip` | S6-01, запасной путь: список задач JSON-файлом → чтение второй сессией → замена с `ifInState` (прежнее состояние — `stateMismatch`) → `FileNode/changes` → удаление (п. 42) |
| `stand_tasks::delegation_round_trip` | S6-01: поручение user1 → user2, наблюдатель user3 (`sendSchedulingMessages`): копии у обоих → принять → в работе 50 % → готово (у поручившего PARTSTAT и процент) → новый срок (`SEQUENCE` 1) → COUNTER письмом (сервер не применяет) → отказ с COMMENT письмом (статус применён, COMMENT — нет) → «забрать»: CANCEL наблюдателю, отказавшемуся — нет (п. 40) |
| `stand_tasks::imip_request_by_mail_lands_in_calendar` | S6-01: поручение своим письмом (iMIP REQUEST с VTODO через `EmailSubmission`, без `sendSchedulingMessages`) → сервер исполнителя сам кладёт VTODO в календарь, письмо — во «Входящих» с темой отправителя (п. 40) |
| `stand_tasks::task_round_trip` | S6-02: хранилище Postman (`tasks::JmapStore`) — два клиента со своими кэшами: создать → у второго по `CalendarEvent/changes` → выполнить вторым (патч к серверной версии) → у первого «выполнена», дата выполнения и проект на месте → удалить → у второго нет |
| `stand_tasks::mail_due_is_task` | S6-03: письмо со сроком у user1 (`Email/set create`) → задача «Из почты» → заметка → задача со ссылкой в хранилище → выполнить → оба слова срока сняты одним `Email/set` (видит второй клиент) → у второго клиента задача выполнена → письмо удалено — сверка снимает ссылку |
| `stand_tasks::task_block_in_calendar` | S6-08: блок «Время в календаре» — событие «занят» (`PT30M`) со ссылкой на задачу (`relatedTo` с `parent`), у задачи на сервере — `X-POSTMAN-BLOCK` → «Убрать» — событие удалено |
| `stand_tasks::alerts_server_mails_only_start_relative_email` | S6-01: напоминания через минуту: письмо «Notification: …» — только `action: email` относительно начала (событие, задача с `start`); по сроку задачи и `display` — ничего (п. 41) |
| `postman-dav stand::dav_discovery` | S7-05а: `.well-known/caldav` → принципал, домашние наборы, входящие и исходящие планирования, адрес участника; календарь (VEVENT и VTODO, ctag, sync-token) и адресная книга (п. 45) |
| `postman-dav stand::caldav_event_round_trip` | S7-05а: PUT с `If-None-Match: *` (повтор — 412) → `sync-collection` после токена → `calendar-multiget` (лишний путь — 404) → `calendar-query` по окну и вне его → правка с `If-Match` (старый ETag — 412) → список ETag и `diff` → неизвестный токен (полный список) → DELETE (старый ETag — 412) → удаление в `sync-collection`, второй DELETE — 404 (п. 45) |
| `postman-dav stand::caldav_free_busy` | S7-05а: событие user2 → календарь открыт user1 (ACL `read` и `read-free-busy`) → POST VFREEBUSY в outbox user1 → занятость user2 ровно на событии; неизвестный адрес — `3.7` без занятости → права и событие убраны (п. 46) |
| `postman-dav stand::caldav_meeting_cycle` | S7-05а: user1 кладёт встречу с ATTENDEE user2 → копия в календаре user2 со Schedule-Tag → ответ правкой копии с `If-Schedule-Tag-Match` → у организатора PARTSTAT=ACCEPTED (старый ETag — 412) → DELETE организатором → у user2 `STATUS:CANCELLED`; входящие планирования: у user2 REQUEST и CANCEL, у user1 REPLY → убраны вместе с письмами сервера (п. 47) |
| `postman-dav stand::carddav_round_trip` | S7-05а: vCard 4.0 PUT → `sync-collection` → `addressbook-multiget` → правка с `If-Match` (старый ETag — 412) → новый ETag в `sync-collection` → DELETE → удаление в `sync-collection` (п. 45) |
| `postman-dav stand::dav_wrong_password` | S7-05а: неверный пароль — `Error::Auth` (401) на поиске, а не «нет CalDAV» |
| `stand_standards::caldav_event_round_trip` | S7-05б, через синхронизацию «стандартов» (IMAP + CalDAV): `Features` — календарь, задачи, контакты, занятость (без справочника и правил) → `CreateMeeting` без участников → неделя (`expand`) → `SearchEvents` → убрано |
| `stand_standards::caldav_meeting_cycle` | S7-05б: user1 → user2 `CreateMeeting` → у user2 в неделе «нужен ответ» → `RespondInvite` своей копией (`Delivery::Server`) → у user1 user2 принял → убрано вместе с входящими планирования и письмами (п. 47) |
| `stand_standards::caldav_free_busy` | S7-05б: событие user2, календарь открыт user1 → `Availability` — занятость user2 ровно на событии, неизвестный адрес — нет данных → `ShareFreeBusy` — ACL только `read-free-busy` (п. 46, 48) |
| `stand_standards::caldav_task_round_trip` | S7-05б: задача с исполнителем user2 → у второго клиента после загрузки → выполнена вторым → у первого по дельте `sync-collection` → удалена → у второго нет; у user2 копии нет (`SCHEDULE-AGENT=CLIENT`, п. 48) |
| `stand_standards::carddav_round_trip` | S7-05б: `SaveContact` → в `ContactsLoaded` → правка (телефон убран, должность и организация) → `DeleteContact` → нет |
| `stand_imap::server_kind_by_address` | S7-04в, S7-05б: адрес JMAP — как есть; к адресу IMAP — `dav=https://<узел>` (`.well-known/caldav` отвечает), с `dav=` — как есть |

Последний прогон — 2026-10-23, после переноса в плагины (S7-02б): `postman-backend-jmap` `stand` 16 из 16, `postman-backend-imap` `stand_imap` 5 из 5 и `stand_standards` 5 из 5, `postman-dav` `stand` 6 из 6; до него, 2026-10-23: `postman-core` `stand_standards` 5 из 5 и `stand_imap` 5 из 5 (S7-05б), `postman-dav` `stand` 6 из 6 (S7-05а, HTTPS напрямую); IMAPS 993 и SMTPS 465 напрямую (`POSTMAN_STAND_IMAP`, `POSTMAN_STAND_SMTP`):
`postman-imap` `stand` 4 из 4 (S7-04а), `postman-core` `stand_imap` 2 из 2 (S7-04б), 5 из 5 (S7-04в: «Отменить», беседа, фильтры, вид сервера); 2026-10-22, напрямую по HTTPS: `stand_tasks::mail_due_is_task` (S6-03), `task_round_trip` (S6-02) и тесты хранения без писем; 2026-10-21: `stand_tasks` 8 из 8 (S6-01; поручение и напоминания — отдельными прогонами, чтобы не упереться в порог входящих, п. 17); `stand_categories` 3 из 3 (корень — `isTopLevel`), `stand_search` 1 из 1
(словоформы с языком `ru:`), `contacts_are_edited_and_directory_is_listed` (ФИО из `description`); 2026-10-20: `stand_send_later` 3 из 3 (S5-06); `stand_labels` 3 из 3 (S5-02); `stand` 13 из 13 и `stand_shared` 3 из 3 (S4-06); ранее 11 из 11 (S4-03); `contacts_through_sync` (user3, S4-02) — отдельно.

## Настройка сервера

Что решается конфигурацией Stalwart, а не кодом клиента (разбор `docs/STALWART-GAPS.md` 2026-10-21). На стенде —
задача владельца стенда; на рабочем сервере — при развёртывании.

| Что | Настройка | Строка GAPS |
|---|---|---|
| Адреса сессии — на публичное имя | переменная окружения контейнера `STALWART_PUBLIC_URL=https://<публичное имя>`; из неё строятся `apiUrl`, `uploadUrl`, `downloadUrl`, `eventSourceUrl` и websocket (заголовок `Host` не используется) | 6 |
| Квота загрузки блобов | синглтон Jmap: `maxUploadCount` (1000), `uploadQuota` (50 МБ), `uploadTtl` (1 ч) | 7 |
| Ограничение входящих | правило «Sender address to recipient throttle» (создаётся при установке): 25 в час на пару «домен отправителя × получатель»; поднять порог или ограничить `match` внешними отправителями | 4 |
| Русские словоформы | `x:Search/set` → `defaultLanguage: "ru-RU"` (и русский в `supportedLanguages`, если его там нет): язык короткого запроса и короткого письма сервер не угадывает и берёт язык по умолчанию. Клиент и так шлёт русские слова с `ru:` | 33 |
| Условие `header` в поиске | `x:Search/set` → `"headers": true` в `indexEmailFields`, затем переиндексация (по умолчанию заголовки не индексируются) | 29 |
| Занятость для всех | `Sharing.maxShares` по умолчанию — 10 получателей на календарь: открывать занятость группе «все», а не каждому коллеге | 3 |
| Сертификат, квоты, NTP, «Архив» в шаблоне ящика | см. строки GAPS | 11, 12, 21, 31 |

## Поведение сервера

1. **Адреса в сессии — внутренние** (даже при входе по HTTPS 443 сессия объявляет `http://…:8080`). `apiUrl`, `downloadUrl`, `uploadUrl`, `eventSourceUrl` абсолютные, на адрес
   из настроек сервера (за туннелем или прокси недоступен). Клиент переносит их на адрес подключения
   (`Session::resolve_urls`): заодно пароль и токен не уходят на другой хост. На сервере — `STALWART_PUBLIC_URL`
   («Настройка сервера»).
2. **Capabilities v0.16.27:** core, mail, submission, vacationresponse, calendars (+parse), contacts (+parse),
   principals, principals:availability, websocket, sieve, blob, quota, filenode, mail:share, emailpush,
   webpush-vapid. `.well-known/jmap` → 307 на `/jmap/session`.
3. **Приглашения — по черновику JMAP Calendars.** Организатор — `organizerCalendarAddress`, участники —
   `participants.*.calendarAddress` (`mailto:`), рассылка — `sendSchedulingMessages: true`. Поля RFC 8984
   `replyTo`/`sendTo` сервер молча игнорирует — приглашения тогда не уходят. Клиент: `NewEvent::to_jmap`.
4. **У участника событие в его аккаунте.** Искать по `uid` (`CalendarEvent/query` с `filter.uid`), себя среди
   участников — по `calendarAddress`. Ответ — патч `participants/<ключ>/participationStatus` с рассылкой;
   организатор видит статус и получает письмо «Accepted: …». Удаление организатором с рассылкой — CANCEL.
5. **Занятость — только после шаринга.** `Principal/getAvailability` отдаёт периоды, если календарь открыт
   запрашивающему (`shareWith/<principal>`: `mayReadFreeBusy` или `mayReadItems`); без шаринга — пустой список
   без ошибки. Для «все видят занятость всех» нужен общий шаринг (группа) — настройка сервера, один раз
   (`Sharing.maxShares` — 10 получателей на календарь, по одному коллеге не хватит).
   Клиент: `share_calendar`, `principal_by_email`, `availability` (с capability `principals:availability`).
6. **CalDAV free-busy (Thunderbird, Apple Calendar)** в v0.16.27 требует сразу оба права, `mayReadFreeBusy` и
   `mayReadItems` (баг сервера; JMAP — любое одно). Отсюда пустая «Доступность» у Apple и Thunderbird при
   шаринге «только занятость» — ответ на вопрос `docs/HANDOFF.md` §4 п. 3–4. Postman ходит по JMAP — не затронут.
7. **Контакты:** сервер сам заполняет `name.full` по компонентам в их порядке (`isOrdered`), клиент показывает
   `full` — порядок «Фамилия Имя» задаётся при создании карточки.
8. **Правка контактов:** `name` только с `full` (без компонентов) сервер хранит как есть — клиент создаёт и правит
   карточку одним полем «Имя». Патч `ContactCard/set` по свойствам целиком (`emails`, `phones`, `organizations`,
   `titles`); `null` убирает свойство. Клиент шлёт только изменённые свойства (`ContactCard::patch`).
9. **Справочник:** `Principal/query` без фильтра отдаёт всех (сотрудники и `admin`);
   `name` — логин (адрес), а ФИО — в `description` («Test user1»). Клиент показывает `description`, без него —
   `name` (`Principal::display_name`: справочник, подсказки адресов, участники и переговорные встречи). Группы и
   записи без адреса клиент не показывает (`postman_core::contacts::directory`).

10. **Push по типам.** Событие `state` перечисляет изменившиеся типы по аккаунтам (`changed.<аккаунт>.<тип>`):
   почта — `Email`, `Mailbox`, `Thread`, `EmailDelivery`; календарь — `CalendarEvent` (и `Calendar`,
   `CalendarEventNotification`). Клиент (`sync::Changed::of`) сверяет почту только на почтовые типы, на календарные —
   шлёт `CalendarChanged` с задержкой 1,5 с на пачку; не разобралось — сверяет всё.
11. **Отказ во входе — HTTP 401** уже на `/.well-known/jmap`. Клиент останавливает синхронизацию (повторы с тем же
   паролем бессмысленны, а сервер блокирует адрес за перебор) и показывает строку входа с адресом и логином.
12. **Квота загрузки блобов** — 1000 файлов или 50 МБ на учётную запись (`overQuota` у `Blob/upload` и `/upload`),
    импортированные письма в ней остаются, пока не истечёт срок временных блобов. Поэтому большой ящик заполняется
    через `Email/set create` (сервер сам собирает MIME), а не `Email/import`. Для пользователей важно: вложения в
    окне письма загружаются блобами — тысячи вложений подряд упрутся в эту квоту. Пределы — синглтон Jmap:
    `maxUploadCount`, `uploadQuota`, `uploadTtl` («Настройка сервера»).
13. **Пачка `Email/set create` на 250 писем** проходит (~380 писем/с с рабочей машины по HTTPS).

14. **Автоответ — Sieve-скрипт `vacation`** (S4-05). `VacationResponse/set` пишет скрипт `vacation` (даты —
   `currentdate` в UTC, текст — MIME base64) и с `isEnabled: true` делает его активным, а прежний активный скрипт
   (правила) — неактивным: активен всегда один скрипт. `isEnabled: false` снимает только `vacation`, другой
   активный скрипт не трогает; тема и текст без `isEnabled` активность не меняют. `isEnabled` в ответе
   `VacationResponse/get` — «скрипт `vacation` активен». Автоответ приходит за секунды. Повтор тому же
   отправителю сервер, по RFC 5230, подавляет — тест берёт новую тему на каждый прогон [проверить срок подавления].
14а. **Свой скрипт `vacation`** (S5-29): Stalwart v0.16 запрещает его запись клиентом: `SieveScript/set` отвечает `forbidden`
   («use VacationResponse/set instead»). На таком сервере автоответ — один текст всем: разные тексты и круг адресатов не
   применяются, «Вне офиса» убирается, панель показывает причину (`stand_settings::autoreply_split_texts`). На серверах,
   где запись разрешена, Postman пишет свой скрипт: коллегам — `address :domain :is "from"`, внешним — `address :is "from"
   [контакты]` или все, `:days`, `:addresses`, `:mime` (текст и HTML), период — `currentdate`.

14б. **Индекс поиска.** Stalwart не находит начало темы в `[скобках]` (оно уходит из имени цепочки) и длинные числа, а
   `SEARCH HEADER Message-ID` с индексом заголовков ищет по словам, поэтому Postman сверяет `Message-ID` точно.

15. **Квота** (RFC 9425): у пользователей стенда `Quota/get` — пустой список (ограничений нет), строка состояния без
   заполненности; ящик с квотой — [проверить] (на стенде не задана).
16. **Правила — Sieve** (RFC 9661, S4-07). Скрипты — blob: загрузка, затем `SieveScript/validate` по `blobId`
    (ошибка — `invalidScript` с местом: «… at line 4, column 1») и `SieveScript/set` (неверный скрипт —
    `notCreated`/`invalidScript`); `blobId` записанного скрипта — свой, не загруженный. Активен один скрипт:
    `onSuccessActivateScript` переключает, `onSuccessDeactivateScript` — ни одного; активный не удаляется.
    `fileinto :mailboxid` требует ещё `require "mailbox"` (иначе «Undeclared capability 'mailbox'»). Адрес
    `redirect` сервер не проверяет (пустой принят) — проверяет клиент. `include :personal :optional "vacation"`
    принят, и автоответ из подключённого скрипта работает при активных правилах (тест). Postman пишет скрипт
    `postman` (`crates/core/src/rules.rs`); при включённом автоответе и правилах — `include`, `isEnabled` сервера тогда
    `false`, клиент считает автоответ включённым по своему скрипту. Другие скрипты — только просмотр; замена
    активного — после «Заменить».

17. **Ограничение входящих на получателя** (2026-10-20): после десятков писем одному адресу за короткое время
    Stalwart пишет в журнал `smtp.rate-limit-exceeded` и **молча** не доставляет следующие, в том числе приглашения
    (iTIP): у отправителя `CalendarEvent/set` успешен, у получателя ни события, ни письма. Тесты стенда, которые шлют
    user2 много писем (`stand`, `stand_settings`), лучше не гонять подряд; при сбое `invitation_round_trip` — смотреть
    журнал сервера. Для клиента: «приглашение отправлено» не значит «доставлено». Порог — правило «Sender address to
    recipient throttle», созданное при установке: 25 в час на пару «домен отправителя × получатель» («Настройка
    сервера»); приглашение при этом теряется насовсем — ошибка сервера (GAPS 4).
    2026-10-23: `stand_standards::caldav_meeting_cycle` один раз не дождался приглашения у user2 после десятка прогонов
    `stand_standards` и `stand` за час — по счёту писем «itt.test × user2» порог пройден; журнал сервера не смотрели
    [проверить: `smtp.rate-limit-exceeded`]. Тест в сообщении о сбое называет эту причину; правка порога — пункт 3
    «Настройки стенда» у владельца, ещё не применена.

18. **Общий ящик группы** (S4-06). Группа (`x:Account/set` `@type: Group`; участник — `memberGroupIds` у пользователя)
    даёт участнику в `session.accounts` аккаунт группы: `isPersonal: false`, `isReadOnly: false`, capabilities почты
    и отправки; папки — свои (`Inbox`, `Sent Items`…), `myRights` — полные. Письмо на адрес группы ложится во
    «Входящие» аккаунта группы (участникам не рассылается). id папок и писем — в пределах аккаунта и совпадают с id
    своего ящика (`a`, `b`…): клиент хранит в кэше ключи `<аккаунт>/<id>` (`postman_cache::scoped`), свои — без префикса.
19. **Папка, открытая другому** (`Mailbox/set` `shareWith: { <principal>: { mayReadItems, maySetSeen } }` у владельца):
    у получателя в сессии — аккаунт владельца, в нём только эта папка; `isReadOnly` аккаунта — `false` и при шаринге
    «только чтение», права — только в `myRights` папки. Stalwart ставит и `maySetKeywords: true`, хотя его не давали:
    в его ACL `maySetSeen` и `maySetKeywords` — одно право (GAPS 9); `mayAddItems`, `mayRemoveItems`, `maySubmit` — `false`. Клиент: папка без
    добавления и удаления — «только чтение» (удалять нельзя, флаги — по своим правам), ветка, где так все папки, —
    с подписью «только чтение».
20. **Отправка от имени общего ящика.** В аккаунте группы `Identity/get` отдаёт Identity с адресом группы;
    `EmailSubmission/set` в аккаунте группы с ней проходит: у получателя «От» — адрес группы, копия — в «Отправленных»
    группы. Ещё Stalwart заводит участнику Identity с адресом группы в его собственном аккаунте (отправка оттуда тоже
    проходит, но копия — в свои «Отправленные»); клиент для адреса группы берёт Identity её аккаунта
    (`sync::senders`). Чужой ящик без группы (открыта папка) — `Identity/get` → `forbidden` («You are not an owner of
    account …»): отправлять от его имени нельзя, в «От» его нет. «От имени» с заголовком `Sender` (делегирование через
    право `submit` на папку) — [проверить].
21. **Push по аккаунтам.** EventSource участника присылает и `changed.<аккаунт группы>`: письмо в ящик группы приходит
    в кэш за ~1 с (тест). Для папки, открытой другим пользователем, — тоже по аккаунту владельца [проверить]. Клиент
    сверяет только названные аккаунты (`sync::MailChange`), опрос раз в 2 минуты — все.
22. **Беседы** (S5-05, тест `stand_threads`). Ответ с `In-Reply-To`/`References` и темой «RE: …» Stalwart кладёт в ту же
    цепочку, что и исходное письмо, у обоих пользователей (`threadId` — свой в каждом аккаунте), вместе с копией в
    «Отправленных». `Thread/get` отдаёт `emailIds` из всех папок; `Email/get` со ссылкой
    `#ids: /list/*/emailIds` — одним запросом. `receivedAt` — с точностью до секунды: письма одной секунды идут в
    порядке id. Поиск `Email/query` `text: "RE: …"` не находит ничего — искать без «RE:» и сверять тему на клиенте.

23. **Ключевые слова** (S5-02). Stalwart возвращает их как прислали; заглавные не приводит к нижнему регистру:
    `pm.remind.2026-10-29T0900` и `…t0900` — два разных слова у одного письма (п. 35; RFC 8621 §4.1.1 — без учёта
    регистра; клиент шлёт нижний регистр), `~` и `/` в слове принимает (путь патча — JSON Pointer с `~0`/`~1`). Патч `keywords/<слово>` меняет одно слово,
    остальные (`$seen`, чужие) остаются. `Email/query` с `hasKeyword`/`notKeyword` — и с `inMailbox`, и без него;
    `limit: 0` с `calculateTotal` даёт число без id — счётчики «N из M» одним запросом (до `maxCallsInRequest` = 16
    вызовов; `maxObjectsInSet` = 500). Неизвестный id в `Email/set update` — `notUpdated` с `notFound`, не ошибка запроса.
24. **FileNode** (`urn:ietf:params:jmap:filenode`, черновик IETF): есть у своего аккаунта и у аккаунтов общих ящиков.
    `FileNode/set create` с `blobId` загруженного blob, `parentId: null` — файл в корне; `FileNode/get` отдаёт свой
    `blobId` (не загруженный), имя, тип, права; `destroy` удаляет. Таблица категорий на сервере — здесь
    (`docs/specs/S5.md`, решение №4, S5-02б; п. 29–32). Файл виден в «Файлах» (WebDAV) пользователя — п. 29.
25. **Отложенная отправка — FUTURERELEASE** (S5-06, тест `stand_send_later`). У аккаунта в `submission`:
    `maxDelayedSend: 2592000` (30 дней), `submissionExtensions`: `FUTURERELEASE`, `SIZE`, `DSN`, `DELIVERYBY`,
    `MT-PRIORITY` (`MIXER`), `REQUIRETLS`. Срок задают параметры `envelope.mailFrom.parameters`: `HOLDFOR` (секунды) или
    `HOLDUNTIL` (UTC, `2026-10-21T06:00:00Z`). Свой `sendAt` в `EmailSubmission/set create` сервер не принимает
    (`notCreated`, `invalidProperties` `["sendAt"]`: по RFC 8621 он ставится сервером). В ответе `created` —
    `sendAt` (срок выпуска) и `undoStatus: pending`, `deliveryStatus` — `queued`, «250 2.1.5 Queued».
26. **Предел задержки — 7 дней, а не объявленные 30.** `HOLDFOR` 700000 → `notCreated` `forbiddenMailFrom`
    «501 5.5.4 Requested hold time exceeds maximum of 604800 seconds», 604800 — принят. `HOLDUNTIL` в прошлом →
    `forbiddenMailFrom` «501 5.5.4 HOLDUNTIL must be a date and time in the future». Клиент берёт меньшее из
    `maxDelayedSend` и 7 дней (`send_later::max_delay`) и проверяет срок до отправки.
27. **Отмена — `undoStatus: canceled`** (`EmailSubmission/set update`): `updated`, письмо не доставляется (у получателя
    после срока ничего нет). Повторная отмена и отмена после выпуска — `notUpdated` `cannotUnsend` «The requested
    message is no longer in the queue». После выпуска — `undoStatus: final`, но `deliveryStatus.delivered` остаётся
    `unknown` (доставка на том же сервере). `EmailSubmission/query` с `filter.undoStatus: pending` отдаёт ждущие
    отправки — по нему клиент строит список «Отправится …» после перезапуска. Выпуск — через 0–4 с после срока по
    часам сервера (часы стенда на 1–2 с впереди рабочей машины: сверять `receivedAt` с `sendAt`, а не с часами
    клиента). Переживает ли ждущая отправка перезапуск сервера — [проверить].
28. **`onSuccessUpdateEmail` — при создании отправки, и отложенной тоже:** письмо сразу в «Отправленных», без `$draft`,
    и после отмены остаётся там же. Поэтому клиент после отмены сам возвращает его в «Черновики» (`$draft`), а
    запланированное показывает в «Отправленных» с меткой «⏱ срок»: держать его в «Черновиках» нельзя — после выпуска
    сервер его не перенесёт. Папки «Исходящие» у Stalwart по умолчанию нет. Клиент (S5-06): обычная отправка —
    `HOLDFOR` 13 с (окно «Отменить» 10 с + запас 3 с на ответ сервера), «Отправить позже» — `HOLDUNTIL`; письмо держит
    сервер, поэтому выход из Postman в окне «Отменить» ничего не теряет — письмо уйдёт в срок.
29. **FileNode: папки и имена** (S5-02б, тест `stand_categories`). Узел без `blobId` — папка (`nodeType: directory`);
    имя с точкой (`.postman`) допустимо (запрещены только `.`, `..`, имена устройств Windows и `/<>:"\|?*` —
    `forbiddenNodeNames`, `forbiddenNameChars` в `accountCapabilities`), имена сравниваются с учётом регистра
    (`caseInsensitiveNames: false`). Второй узел с тем же именем в той же папке — `notCreated` `alreadyExists` с
    `existingId`: так клиент узнаёт, что папку или файл только что создал другой. Непустая папка не удаляется
    (`nodeHasChildren`), с `onDestroyRemoveChildren: true` — вместе с содержимым. **Файлы видны пользователю:** папка
    `.postman` и `categories.json` — в WebDAV (`PROPFIND /dav/file/<ящик>/`, Depth 1); скрытых или служебных
    узлов у FileNode нет (`role: null`). Пользователь может удалить таблицу — Postman тогда вернётся к умолчанию.
30. **`FileNode/query`:** фильтр `name` — точное совпадение (без подстроки, с учётом регистра), `parentId: "<id>"` —
    работает; корень — `isTopLevel: true` (draft-ietf-jmap-filenode-18; узел с тем же именем во вложенной папке в ответ
    не попадает — тест `stand_categories`). `parentId: null` отвергается по спецификации (`parentId` — тип `Id`;
    сейчас `invalidArguments`), `hasParentId` из прежних версий черновика в draft-18 нет — сервер молча игнорирует
    неизвестное условие (GAPS 24). Клиент: `Client::file_node`.
31. **Замена содержимого и `ifInState`.** `FileNode/set update { blobId }` заменяет содержимое (у узла — свой новый
    `blobId`, `modified` меняется); `ifInState` у `FileNode/set` поддержан: не то состояние — ошибка метода
    `stateMismatch`. Состояние — одно на все файлы аккаунта: любая правка в «Файлах» пользователя сдвигает его, и
    запись таблицы тогда перечитывает и сводит заново (`categories::save`, до 5 попыток). Push: `StateChange` несёт
    `changed.<аккаунт>.FileNode` — и своего, и общего ящика; правка таблицы другим клиентом доходит за 29 мс – 1 с.
32. **FileNode в чужих аккаунтах.** Общий ящик группы (`shared-sales`): файлы свои, права полные
    (`mayCreateTopLevelFileNode: true`), загрузка blob в аккаунт ящика (`/jmap/upload/<аккаунт>/`) работает — у ящика
    своя таблица. Аккаунт пользователя, открывшего user3 папку (`shared-ro`): сессия объявляет у него FileNode
    (`mayCreateTopLevelFileNode: false`), но любой метод FileNode — `forbidden` «You do not have access to account j».
    Клиент считает, что таблицы у такого аккаунта нет (`Reply::Unsupported`), и берёт свою.
33. **Метки и версия событий** (S5-11, тест `event_keywords_sequence_and_principal_kinds`). `keywords` и `categories`
    (`CalendarEvent/set create`) хранятся и отдаются как есть, если их запросить в `properties` — по ним клиент
    красит блок недели цветом категории (`Categories::for_event`: `pm.cat.<id>` или имя категории). `sequence`:
    без рассылки правка его не меняет, нулевой сервер не отдаёт (клиент считает 0); правка организатором с
    `sendSchedulingMessages: true` поднимает его на 1 и у организатора, и в копии участника — по нему карточка
    приглашения видит «изменено» и «устарело» (`invite::classify`).
34. **Переговорные и ресурсы.** На стенде `Principal/query` отдаёт только `individual` и `group` (9 записей): типов
    `location`/`resource` нет — строка «Переговорная» во вкладке встречи на стенде не появляется (в демо — есть).
    Бронирование переговорной (участник-ресурс, автопринятие) и её занятость через `Principal/getAvailability` —
    [проверить], когда администратор заведёт такую запись.

35. **Сроки жизненного цикла** (S5-04, тест `stand_lifecycle`). Слова `pm.due.2026-10-29`, `pm.due.2026-10-29t1500`,
    `pm.wait.2026-10-29`, `pm.remind.2026-10-29t0900`, `pm.expire.2026-10-29t2359` и общие `pm.due`, `pm.wait`,
    `pm.remind`, `pm.expire` ставятся одним патчем, читаются тем же текстом и находятся `hasKeyword` (и по слову с датой,
    и по общему — `hasKeyword` ищет слово целиком, поэтому общее слово ставится рядом). Двоеточие (`t09:00`) сервер тоже
    принимает, но формат Postman — без него. Заглавная `T` — отдельное слово (п. 23).
    **Рассылки:** условие `header` в `Email/query` (RFC 8621 §4.4.1) не находит ни одного письма — ни `["List-Unsubscribe"]`,
    ни `["Subject"]`, ни с значением, и через 10 с после загрузки (тест `header_filter_finds_nothing`); ошибки нет:
    заголовки по умолчанию не индексируются — нужен `headers` в `Search.indexEmailFields` и переиндексация («Настройка
    сервера»). `header:List-Id:asText` — `null`, сырой `header:List-Id` и `header:List-Unsubscribe` — есть (и у письма
    из `Email/import`, и из `Email/set create`: заголовок сохраняется, `null` — только при чтении `asText`, GAPS 30).
    Поэтому рассылки узнаются по сырым заголовкам в `Email/get` (`Client::list_letters`). `before` работает.
    **«Архив»:** у ящиков стенда папки с ролью `archive` нет; `Mailbox/set create` с `role: "archive"` принят — Postman
    создаёт «Архив» при первой правке, которой он нужен («Напомнить», рассылки, «Исчезнет» в архив). Перенос между
    папками и `$seen` — одним `Email/set` патчем (`mailboxIds/<id>`, `keywords/<слово>`).
36. **«Сообщить о фишинге»** (S5-08, тест `stand_phishing`). Папка с ролью `junk` («Junk Mail») у ящиков стенда есть.
    Один `Email/set` патч переносит письмо туда и ставит `$phishing` и `$junk`, снимает `$notjunk`; слова хранятся
    как есть, `hasKeyword $phishing` («Угрозы», «Все папки») находит письмо сразу; обратный патч («Отменить»)
    возвращает папку и слова. Пересылка вложением: `Email/get` отдаёт `blobId` всего письма, скачивание по нему с
    `accept=message/rfc822` — сырое письмо; часть `message/rfc822` (mail-builder кодирует её base64) сервер принимает,
    получатель видит вложение с типом `message/rfc822`. Обучение спам-фильтра Stalwart переносом в «Junk Mail» —
    [проверить] (зависит от настройки сервера).

36. **Поиск «Везде»** (S5-10, тест `stand_search`). Условия запроса командной строки (`postman_core::query`) — `from`
    (адрес и имя), `to`, `subject`, `notKeyword: $seen`, `after`/`before` (UTCDate), `text` — находят ровно ожидаемое.
    Но: **`text` из нескольких слов** ищет все слова в одном поле — «<метка из темы> квартала» (слово из текста письма)
    не находит ничего, а те же слова двумя условиями `text` в `AND` — находят; поэтому Postman шлёт по `text` на слово.
    **Словоформы — с языком запроса.** Язык короткого запроса сервер не угадывает и берёт `Search.defaultLanguage`
    (на стенде en-US): «договоры» без языка не находит «договору». С префиксом языка (синтаксис Stalwart) —
    находит: `text`/`body: "ru:договоры"` и `"ru:договоров"` → «договору», `"ru:поставкой"` → «поставки». Поэтому
    Postman шлёт слова и тему с кириллицей с `ru:` (`query::with_language`), латиницу — как есть. Язык письма сервер
    определяет по его тексту: у письма в одну фразу словоформы не находятся и с языком (индекс — языком по умолчанию),
    тест берёт текст в несколько предложений. В теме с языком находится точная форма (`subject: "ru:договор"`), а
    словоформа — нет (`"ru:договора"`) [проверить]. Свой индекс (tantivy, Snowball) находит словоформы и без сервера.

37. **Задачи — `CalendarEvent` с `"@type": "Task"`** (S6-01, тест `stand_tasks`). `urn:ietf:params:jmap:tasks`
    (draft-ietf-jmap-tasks) не объявлен, но JMAP Calendars Stalwart хранит JSCalendar-задачи в обычном календаре: это
    тот же VTODO, что видит CalDAV (`/dav/cal/<адрес>/default/`, `supported-calendar-component-set` — VEVENT, VTODO,
    VJOURNAL…), и наоборот — VTODO, положенный по CalDAV, в JMAP — `@type: Task`. У JMAP `Calendar` признака
    компонентов нет: задачи и события в одном календаре, и `CalendarEvent/query` с `after`/`before` отдаёт и те, и
    другие — сетка календаря отбирает `@type: Event` сама.
38. **Поля задачи** (JSCalendar 2.0, draft-ietf-calext-jscalendarbis, ↔ iCalendar). Хранятся и отдаются как есть:
    `title` (SUMMARY), `description` (DESCRIPTION, Markdown — как текст), `start` (DTSTART), `due` (DUE), `timeZone`
    (TZID), `estimatedDuration` (ESTIMATED-DURATION, «оценка»), `priority` (PRIORITY; «!» — 1), `percentComplete`
    (PERCENT-COMPLETE), `progress` (STATUS: `needs-action`, `in-process`, `completed`, `failed`, `cancelled`),
    `keywords` (CATEGORIES — категории Postman), `categories` (CONCEPT), `recurrenceRule` (RRULE), `alerts` (VALARM),
    `relatedTo` с `parent` (RELATED-TO;RELTYPE=PARENT — подзадача), `links` (`rel: describedby` → URL; ATTACH →
    `rel: enclosure`; ссылка на письмо — `mid:<Message-ID>`), `showWithoutTime`. Чего у JSCalendar нет — в
    `iCalendar.properties` (jCal: `["x-postman-project", {}, "unknown", "vec"]`; туда же COMMENT и дата COMPLETED):
    отдаётся, только если назвать его в `properties`, пишется из JMAP (X-POSTMAN-* в VTODO), патч `iCalendar` заменяет
    весь список. **Не принимаются** (`invalidProperties`): `recurrenceRules` (массив RFC 8984; в JSCalendar 2.0 —
    `recurrenceRule`), `progressUpdated`, свойства поставщика (`example.com:project`, RFC 8984 §3.3). Дату выполнения
    (COMPLETED) сервер не ставит и в JMAP не переводит — клиент пишет её сам (`iCalendar`). VTODO, созданный через
    JMAP, в CalDAV — с `TZID`, но без VTIMEZONE, как и события из JMAP (у копии из iTIP VTIMEZONE есть; GAPS 44).
39. **Изменения и отбор задач.** `CalendarEvent/changes` отдаёт создание, правку и удаление задачи второму клиенту
    (push — тип `CalendarEvent`, п. 10); CalDAV — `sync-collection` (удаление — 404 в ответе), ETag и `If-Match`
    (старый — 412). Отбор на сервере: по сроку — `after`/`before` (у задачи без `start` `utcStart` — срок, `utcEnd` —
    конец того же дня по её поясу), `title`/`text`; **по статусу и типу записи — нет** (`progress`, `status`, `type` —
    `unsupportedFilter`), сортировки по сроку тоже (`due` — `unsupportedSort`; по `start` — есть). CalDAV
    `calendar-query` умеет и то и другое: `time-range` по VTODO и `prop-filter STATUS` (с `negate-condition`). Фильтр
    JMAP `uid` по префиксу ничего не находит (остатки тестов ищутся CalDAV `text-match`) [проверить: в
    `Client::event_by_uid` он описан как подстрока].
40. **Поручение — iTIP силами сервера** (`sendSchedulingMessages: true`, тест `delegation_round_trip`). Задача с
    `organizerCalendarAddress` и `participants` (исполнитель — `expectReply`, наблюдатель — `roles: informational` →
    `ATTENDEE;ROLE=NON-PARTICIPANT`) доходит за миллисекунды: копия сама ложится в календарь исполнителя **и
    наблюдателя** (как VEVENT, п. 4), письмо сервера «Invitation: …» с `text/calendar` и `event.ics`; X-POSTMAN-*
    приходят в копию. Ответы исполнителя — патч своей копии с рассылкой: `participationStatus: accepted` → у
    поручившего «принято»; `participants/<я>/progress` `in-process` / `completed` вместе с `progress` и
    `percentComplete` задачи → у поручившего PARTSTAT=IN-PROCESS / COMPLETED, STATUS и PERCENT-COMPLETE. Каждый такой
    патч — отдельное письмо поручившему («Accepted: …», «Reply: …»), в пороге входящих (п. 17). Процент участника
    (`participants/<я>/percentComplete`, компонент PARTICIPANT) остаётся у исполнителя. Правка поручившим (срок) —
    REQUEST с `SEQUENCE` 1 и письмо «Updated invitation: …» исполнителю и наблюдателю. **Наблюдатель получает всё**
    (каждый REQUEST и CANCEL): «только итоги» (решение 9) сервер не умеет. Явный `roles: { attendee: true }` сервер
    пишет лишним `JSPROP;JSPTR=…` — роль по умолчанию лучше не указывать. Письмом (iMIP, `EmailSubmission`):
    **COUNTER** («предложить срок») доходит как письмо, копию поручившего сервер не меняет; **REPLY** `DECLINED` сервер
    применяет, но **COMMENT** («причина отказа») теряет. «Забрать» — удаление с рассылкой: CANCEL наблюдателю (у него
    `progress: cancelled`, запись остаётся), отказавшемуся исполнителю — нет. **Удаление копии участником** по CalDAV (`DELETE`) сервер считает отказом: поручившему
    уходит REPLY «Declined: …» (так и у наблюдателя); JMAP `destroy` с `sendSchedulingMessages: false` — молча. Клиенту:
    «удалить у себя» поручение без отказа — только JMAP без рассылки. Письма-приглашения сервера — его шаблон
    (тема «Invitation: …», логотип), не HTML Postman (решение 3). Но и **своё письмо** (iMIP REQUEST с VTODO через
    `EmailSubmission`, без `sendSchedulingMessages`; тест `imip_request_by_mail_lands_in_calendar`) сервер исполнителя
    разбирает так же: VTODO ложится в его календарь сам, письмо — во «Входящих» с темой и текстом отправителя.
41. **Напоминания.** VALARM / `alerts` хранятся как есть (и у задач). Сервер сам шлёт письмо «Notification: <тема>
    @ <время>» от `calendar-notification@<домен>` только для `action: email` относительно начала — у события и у
    задачи со `start`; в тесте — через ~55 с после создания (срабатывание через 60 с). По сроку задачи
    (`relativeTo: end`) и для `display` — ничего. Значит, напоминание по сроку — только клиентом (решение 7); письмо
    сервера — разве что дополнительным каналом, если у задачи есть начало [проверить: настройки писем напоминаний].
42. **Задачи в FileNode** (запасной путь): JSON-файл пишется, читается второй сессией, заменяется с `ifInState`,
    `FileNode/changes` отдаёт его второму клиенту (как таблица категорий, п. 29–31). Но это файл, видимый и удаляемый
    пользователем (п. 29), без отбора на сервере, без iTIP и без показа в других клиентах — только если календарь
    недоступен.
43. **Правило «→ категория» — `addflag` (`imap4flags`, S5-26).** Скрипт `if header :contains "subject" "…"
    { addflag "pm.cat.tenders"; }` с `require ["imap4flags"]` сервер принимает (`SieveScript/validate`), и новое
    письмо приходит во «Входящие» с ключевым словом `pm.cat.tenders` в `keywords` (тест `sieve_rule_sets_category`,
    `stand_settings.rs`). Своё ключевое слово (не системный флаг) ставится как есть, регистр сохраняется.

44. **Поручение путём Postman** (S6-09, тест `postman_delegation_cycle`, 2026-10-22). Своя запись поручившего —
    `CalendarEvent/set` **без** рассылки, письма — свои iMIP (REQUEST исполнителю и наблюдателю, REPLY исполнителя с
    COMMENT). Сервер исполнителя кладёт VTODO из письма в его календарь сам (как п. 40); **REPLY письмом сервер
    поручившего применяет и к записи, созданной без рассылки**: PARTSTAT (ACCEPTED, IN-PROCESS, COMPLETED) и
    PERCENT-COMPLETE — у участника; ход самой задачи (`progress`) не меняет — «готово» исполнителя у поручившего
    остаётся «на проверку» до приёмки. REQUEST поручившего с `STATUS:COMPLETED` («принять результат») меняет
    `progress` копии исполнителя на `completed`, но своё `X-POSTMAN-RESULT` из письма в копию не переносит (свойства
    X- первого REQUEST доходят, п. 40) — «закрыто» Postman узнаёт по `progress` + PARTSTAT COMPLETED
    (`assign::status`). COMMENT — по-прежнему только в письме (GAPS 42): Postman берёт его в ленту шагов при открытии
    письма.

45. **CalDAV и CardDAV** (S7-05а, `crates/dav/tests/stand.rs`, 2026-10-23). `.well-known/caldav` и `/carddav` —
    307 на `/dav/cal` и `/dav/card` (метод PROPFIND сохраняется), корень `/` — 302. Принципал `/dav/pal/<адрес>/`;
    наборы `/dav/cal/<адрес>/` и `/dav/card/<адрес>/`; планирование — `/dav/itip/<адрес>/inbox/` и `/outbox/`;
    `calendar-user-address-set` — `mailto:<адрес>`; `schedule-default-calendar-URL` — 404 (приглашения ложатся в
    `default`). У пользователя один календарь `default` (VEVENT, VTODO, VJOURNAL, VFREEBUSY, VAVAILABILITY…) и одна
    книга `default`; у обоих `getctag` и `sync-token`; `calendar-color` и описание — 404. `calendar-data` и
    `address-data` — в CDATA. PUT — 201 с ETag; `If-None-Match: *` на существующий — 412, старый `If-Match` у PUT и
    DELETE — 412. `sync-collection`: без токена — полный список **вместе с самой коллекцией** (у неё свой ETag);
    после токена — только изменения, удалённое — 404 у ресурса; без изменений — один новый токен. **Неизвестный
    токен — 207 с полным списком**, а не 403 `DAV:valid-sync-token` (RFC 6578 §3.2): удалённое за это время клиент
    не увидит. Клиент: полный список узнаёт по самой коллекции в ответе (`Changes::full`) и сверяет по ETag
    (`Client::etags`, `diff`). `calendar-multiget` по лишнему пути — 404 у ресурса; `calendar-query` с `time-range`
    — на сервере.
46. **Занятость по CalDAV** (RFC 6638 §5, тест `caldav_free_busy`). POST VFREEBUSY (`METHOD:REQUEST`, ORGANIZER —
    свой адрес) в свой outbox → `schedule-response` по участнику: `2.0;Success` и VFREEBUSY с `FREEBUSY;FBTYPE=BUSY`
    (периоды через запятую, строка свёрнута по 75 октетов); чужой адрес — `3.7;Invalid calendar user or
    insufficient permissions` без данных. **Без прав на календарь коллеги — тоже `2.0` и пустой VFREEBUSY**: не
    отличить от «свободен» (RFC 6638 ждёт `3.8;No authority`). Права — WebDAV ACL на календарь коллеги (метод `ACL`
    принимается, к правам сервер дописывает `read-current-user-privilege-set`): занятость — только при `read` **и**
    `read-free-busy` сразу; по одному — пусто (п. 6 подтверждён по CalDAV). Клиент: `Client::share`/`unshare`.
47. **Неявное планирование по CalDAV** (RFC 6638 §3, тест `caldav_meeting_cycle`). PUT события с ORGANIZER (свой
    адрес) и ATTENDEE — сервер за доли секунды кладёт копию в `default` участника и пишет письма «Invitation: …»
    (по два письма на каждого за цикл). Schedule-Tag — в ответе PUT организатора и у копии (`calendar-query`,
    GET). Ответ участника — PUT своей копии с `If-Schedule-Tag-Match` и PARTSTAT=ACCEPTED: принят, Schedule-Tag
    копии не меняется (RFC 6638 §3.2.10), у организатора PARTSTAT=ACCEPTED (его ETag меняется — старый 412). DELETE
    организатором — CANCEL: копия участника остаётся со `STATUS:CANCELLED`, удаляет её сам участник. Входящие
    планирования: у участника — REQUEST и CANCEL, у организатора — REPLY; PROPFIND их перечисляет, GET отдаёт, но
    **`calendar-multiget` и `calendar-query` по входящим — 404** на каждый ресурс. Клиент: `Client::inbox` читает
    недостающее GET по одному.
48. **CalDAV для Postman** (S7-05б, тест `stand_standards`). `calendar-query` с `<C:expand>` (RFC 4791 §9.6.5) —
    поддерживается: экземпляры повтора — отдельные VEVENT с `RECURRENCE-ID` и временем в UTC (неделя Postman — как
    у JMAP). `SCHEDULE-AGENT=CLIENT` у ATTENDEE соблюдается: VTODO с исполнителем сервер не рассылает и копию
    исполнителю не кладёт (письма поручения шлёт Postman). Чужой принципал (`/dav/pal/<адрес коллеги>/`) — PROPFIND
    отвечает 403, PROPFIND `/dav/pal/` Depth 1 — только свой: найти коллегу по адресу CalDAV нельзя. Клиент берёт
    путь по образцу своего (`dav::sibling_principal`), ACL на него сервер принимает. Где по CalDAV видны календари,
    открытые мне коллегами, — [проверить] (неделя Postman по CalDAV их пока не показывает).
49. **Исключение экземпляра серии по CalDAV** (S7-07б, тест `caldav_meeting_edit_and_cancel`). Ресурс с исключением
    (`RECURRENCE-ID`, RFC 5545 §3.8.4.4) сервер хранит как есть, но `calendar-query` с `<C:expand>`: (1) пока у серии
    нет `EXDATE`, отдаёт и исключение, и прежний экземпляр на его месте — в неделе два экземпляра вместо одного; с
    любым `EXDATE` у серии — верно; (2) у исключения отдаёт `RECURRENCE-ID`, равный его новому началу, а не исходному.
    По JMAP (`recurrenceOverrides`) — верно. Клиент: исключение ищется и по началу (`ics::instance_rid`), правка
    исключения повторно работает; дубль в неделе — от сервера. В `docs/STALWART-GAPS.md` — №49.
    `recurrenceRules` (RFC 8984) в `CalendarEvent/set` — `invalidProperties`, повтор — `recurrenceRule` (JSCalendar
    2.0); путь внутрь пустой карты `recurrenceOverrides/<id>` — `invalidProperties`, карта — целиком.

## S/MIME (S5-12 Ф2)

- Шифрованное письмо (`application/pkcs7-mime; smime-type=enveloped-data`) сервер пропускает как есть: блоб письма у
  получателя побайтно тот же, расшифровывается (`stand_smime::encrypted_mail_survives_the_server`, 2026-10-22).
- `Email/get` со свойством `header:Content-Type:asRaw` Stalwart отвечает ключом `header:Content-Type` (без формы) —
  Postman просит `header:Content-Type` (форма Raw — по умолчанию, RFC 8621 §4.1.3).

44. **Календарь коллеги (S5-22).** Календарь user2, открытый user1 (`Calendar/set` `shareWith/<principal>`:
    `mayReadItems: true`), появляется в сессии user1 отдельным аккаунтом с capability календарей; имя аккаунта —
    адрес владельца (`user2@itt.test`). `Calendar/get` и `CalendarEvent/query` с этим `accountId` отдают его календарь
    и события (тест `shared_calendar_is_visible`). Закрыть — `shareWith/<principal>: null`.

## Общие ящики на стенде (S4-06)

- `shared-sales@itt.test` — группа (`x:Account/set` от имени администратора), user3 — участник; пароля нет.
- `shared-ro@itt.test` — пользователь (пароль — в файле владельца рядом с остальными); его «Входящие» открыты user3
  только для чтения (`Mailbox/set` `shareWith` на principal user3).
- Смотрит user3; тест — `cargo test -p postman-backend-jmap --test stand_shared -- --ignored`, по одному письму на прогон и
  адрес (п. 17).
- «Взять в работу» (S5-28): слова `pm.taken` и `pm.taken.2026-10-22t1551z.user3@itt.test` (с `@` и временем)
  Stalwart хранит как есть, у письма ящика группы — для всех с доступом (`taken_words_survive_the_server`, 2026-10-22,
  зелёный). Двое участников (`taken_is_seen_by_a_colleague`: user3 берёт — коллега видит, коллега забирает — у user3
  в кэше по push) — [проверить]: в группе один участник, нужен ещё user1 или user2 (`x:Account/set` от имени
  администратора, задача стенда).

## Большой ящик (S4-03)

Учётная запись `big@itt.test` (создана `x:Account/set` от имени администратора; пароль — в файле владельца рядом с
остальными) заполнена 25 000 писем примером `fill_stand` (содержимое демо-ящика: отправители, темы, текст; даты — в
прошлое, папки с ролью `archive` у стенда нет — всё во «Входящих»):

```
POSTMAN_URL=https://<стенд> POSTMAN_INSECURE_TLS=1 POSTMAN_USER=big@itt.test POSTMAN_PASSWORD=…
cargo run --release -p postman-backend-jmap --example fill_stand -- 25000        # дополняет до N, ~65 с на 24 тыс.
cargo run --release -p postman-gpui -- --bench --live [--warm] [--keep]  # замер живого ящика
```

`--bench --live` — тот же сценарий, что и `--bench` (`crates/core/src/bench.rs`), но на учётной записи из окружения;
кэш и индекс — в `%LOCALAPPDATA%\Pets\testdata\postman\live-bench` (рабочий кэш не трогается). Без `--warm` — с
пустого кэша (первая синхронизация), `--keep` оставляет кэш для следующего прогона с `--warm` (запуск с кэша).
Сценарий начинается после первой синхронизации; в строке итога ещё `first_sync_ms` (запуск → «Синхронизировано») и
`list_ms` (запуск → первый кадр со строками). При листании список догружает папку страницами по 500 (`LoadMore`).

Замеры 2026-10-20, release, Windows 11, RTX 5060, стенд по HTTPS из офисной сети; для сравнения — демо 50 тыс. на той
же сборке:

| Что | Бюджет (§2c) | Живой, пустой кэш | Живой, с кэша (`--warm`) | Демо 50 тыс. |
|---|---|---|---|---|
| Первая синхронизация (`first_sync_ms`) | — | 715 мс | 483 мс (дельта `Email/changes`) | — |
| Список на экране (`list_ms`) | < 1 с с кэша | 1152 мс | **424 мс** ✓ (первый кадр) | — |
| Первый кадр | — | 418 мс | 424 мс | 469 мс |
| Писем в списке к концу сценария | — | 5 500 из 25 000 | 6 000 из 25 000 | 35 165 |
| CPU в простое (`idle_cpu_pct`, push подключён) | ≤ 0,5 % | 0,0 % (3 из 5 прогонов), 0,8–1,2 % | 0,0 % (2 из 3), 0,3 % | 0,0 % |
| Рабочий набор: пик / в простое | ≤ 150 МБ | 225 / 163 МБ ✗ | 251 / 171 МБ ✗ | 230 / 195 МБ ✗ |
| Private bytes: пик / в простое | — | 188 / 132 МБ | 214 / 139 МБ | 195 / 166 МБ |
| Листание с открытием: CPU процесса / потока UI | — | 131 % / 52 % | 126 % / 51 % | 77 % / 58 % |
| Листание списка: CPU процесса / потока UI | — | 83 % / 55 % | 135 % / 26 % | 47 % / 38 % |
| Кадр листания p50 / p99 | — | 6,9 / 9,0 мс | 6,9 / 8,1 мс | 6,9 / 7,9 мс |

Выводы:

- Запуск с кэша — в бюджете. Простой с подключённым push — 0,0 %, когда фон закончен: поток синхронизации спит на
  сокете. 0,3–1,2 % — прогоны, где в окно замера простоя (3 с после сценария + 10 с) ещё попала предзагрузка тел
  последней догруженной страницы и их извлечение в индекс; это работа, а не простой, но окно стоит удлинить или
  ждать конца фона [проверить].
- **Память выше бюджета и у демо** (на этой сборке 230 МБ пик, 195 МБ в простое; 88–90 МБ в §2c — замер спайка
  фазы 0, не всего приложения). Живой ящик не хуже демо: в кэше — только загруженные страницы. Разбор, что держит
  память (атлас gpui, шрифты, область чтения, индекс поиска), — отдельной задачей: S4-08, `docs/MEMORY.md`.
- Листание живого ящика нагружает процесс сильнее демо (126–135 % против 47–77 %) за счёт фона, не потока
  интерфейса: догруженные страницы пишутся в кэш и индекс поиска, тела писем предзагружаются (по 25 за запрос) и
  извлекаются в индекс. Кадр от этого не страдает (p99 ≤ 9 мс).
- Исправлено по замерам: запуск с кэша больше не перечитывает первые 500 писем каждой папки (дельта с прошлого
  состояния; 483 мс против 715 мс и меньше трафика); push календаря не вызывает сверку почты; полный обход кэша для
  предзагрузки тел — не на каждый push, а только после догрузки страницы и после сбоя предзагрузки.

## Не проверено

- Вход через внешний OIDC и автосоздание ящика (`docs/IDENTITY.md`) — на стенде только пароли.
- Письма с внешних доменов (iMIP с Gmail/Exchange) — у стенда нет внешнего домена.
