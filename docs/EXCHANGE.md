# Exchange (on-prem): учётная запись, проверка, находки

Exchange on-prem по EWS, best effort (ADR 0003, ADR 0004, `docs/specs/S7.md`). Почта, календарь, ответы на встречи,
занятость и справочник — всё по EWS (решение владельца 2026-10-23 вечер, `S0-decisions.md`): у Exchange компании
IMAP4 и приём SMTP от клиентов выключены. Плагин — `postman-backend-ews` (`MailPlugin` — почта, `AreaPlugin` с ключом
`ews` — календарь, занятость, GAL), протокол — крейт `postman-ews`. Вход — Negotiate/NTLM через SSPI Windows, логин и
пароль учётной записи (диспетчер учётных данных Windows). Basic и OAuth не поддерживаются.

**Без адресов, доменов и имён:** здесь и в находках — только версии, коды и счётчики.

## Как Postman заводит учётку Exchange

«Добавить аккаунт» (или первый запуск) — существующий экран: адрес сервера, логин, пароль.

1. Вид сервера по адресу (`Registry::detect`): JMAP нет → EWS по введённому узлу без входа: `https://<узел>/EWS/Exchange.asmx`,
   затем `https://autodiscover.<узел>/EWS/Exchange.asmx` (адрес — домен почты) отвечает 401 с Negotiate или NTLM
   (`postman_ews::probe`) → адрес учётной записи — этот URL EWS. Иначе — IMAP на порту 993, как раньше.
2. При входе — Autodiscover POX с логином и паролем (логин — адрес почты): точный URL EWS (часто другой узел, чем
   `autodiscover.<домен>`); не ответил — адрес учётной записи.
3. Вручную: адрес сервера — сам URL EWS (`https://<узел>/EWS/Exchange.asmx`).

Что видно (`Features`): «Почта», «Календарь» (неделя, поиск, встречи, ответы на приглашения, «Помощник
планирования»), «Контакты» — «Мои контакты» (папка «Контакты» Exchange, T1-25) и «Справочник» (поиск по GAL от 3
символов, подсказки адресов). Задач, правил, автоответа, квоты, общих ящиков, меток и категорий Postman у Exchange нет —
их нет и на экране.

| Функция | EWS | Как |
|---|---|---|
| Дерево папок | `SyncFolderHierarchy` с `folder:ParentFolderId` и `folder:FolderClass`, дальше — с прошлого состояния; запасной — `FindFolder` (Deep) под `msgfolderroot` | роли — по стандартным папкам (`GetFolder`: inbox, drafts, sentitems, deleteditems, junkemail, outbox); создать, переименовать, перенести — `CreateFolder`, `UpdateFolder`, `MoveFolder` (T1-04) |
| Письма папки | `FindItem` (новые 500 по дате) + `SyncFolderItems` (изменения, «прочитано») | по событиям сервера (`Subscribe`: streaming, запасной — pull) — сверка только изменённой папки; без подписки — опрос раз в 2 мин, дерево — не чаще раза в 15 мин; «снова на связи» — сразу (T1-21) |
| Список | `GetItem` (`Default` + `item:Flag`) | тема, отправитель, дата, размер, вложения, прочитано, флаг |
| Письмо | `GetItem` `MimeContent` | разбор MIME — воркер-песочница; цепочка — по `References` из MIME |
| Прочитано, флаг | `UpdateItem` (`message:IsRead`, `item:Flag`) | других ключевых слов EWS не хранит |
| Перенос, удаление | `MoveItem`, `DeleteItem` | у перенесённого письма — новый id |
| Отправка, черновик | `CreateItem` из MIME: `SendAndSaveCopy`, `SaveOnly` | копию в «Отправленные» кладёт сервер; «Отменить» держит синхронизация; «Отправить позже» нет |
| Поиск, фильтры | `FindItem` с `QueryString` (AQS) | по папкам; даты и размер в AQS — [проверить] |
| Неделя календаря | `FindItem` + `CalendarView`, `GetItem` | повторы разворачивает сервер: экземпляры и изменённые — своими событиями с общим UID, удалённых нет; та же версия (`ChangeKey`) — без `GetItem`; отменённые не показываются |
| Новая встреча | `CreateItem`, `SendToAllAndSaveCopy` | пояс — Windows id (`postman_ews::tz`, CLDR `windowsZones` целиком; сервер пояс не знает — UTC); UID задаёт сервер |
| Правка, отмена своей встречи | `UpdateItem`, `DeleteItem` с рассылкой (`SendToAllAndSaveCopy`) | «Изменить» и «Отменить встречу» в подробностях (S7-07б, `UpdateMeeting`, `CancelMeeting`): экземпляр серии — исключение сервера, серия — по серии (`RecurringMasterItemId`) со сдвигом на ту же разницу |
| Ответ на приглашение | `AcceptItem` / `TentativelyAcceptItem` / `DeclineItem` | из карточки письма и из календаря — по встрече с UID приглашения (серия — по серии, `RecurringMasterItemId`; приглашение на один экземпляр — ему); нет её — письмом iMIP |
| Письма встреч | `MeetingRequest`, `MeetingResponse`, `MeetingCancellation` — как письма | в списке, MIME с `text/calendar` — карточка приглашения |
| «Помощник планирования» | `GetUserAvailability` | ошибка по адресу — «нет данных», не «свободен» |
| Справочник | `FindPeople` по GAL (папка `directory`), отказал — `ResolveNames` | поиск с подсказкой (решение владельца 2026-10-28, `docs/decisions/2026-10-28-exchange-directory-search.md`): `Command::FindPeople` → `PeopleFound`: имя, адрес, должность, отдел; телефон — `ResolveNames` по адресу при открытии карточки (`Command::PersonDetails`, S7-08б); без строки — пусто, GAL целиком не читаем |
| Мои контакты | папка `contacts`: `FindItem`, `GetItem`, `SyncFolderItems`; `CreateItem`, `UpdateItem`, `DeleteItem` | карточки; списки рассылки — только чтение, в список не идут; поиск своих — по `contacts:FileAs` (T1-25) |

Ограничения сервера: не чаще 5 запросов в секунду; `ErrorServerBusy` — пауза `BackOffMilliseconds` и до 2 повторов.

## Проверка

Стенда Exchange нет: проверка — на боевом Exchange компании, только тестовыми учётками, по правилам S7 «Проверка на
боевом Exchange». Без доступа — тесты на корпусе ответов (по примерам Microsoft, обезличен).

| Что | Где | Запуск |
|---|---|---|
| Разбор ответов, сборка запросов, вход (заголовки, привязка к каналу, SSPI NTLM без сети) | `crates/ews` | `cargo test -p postman-ews` |
| Плагин против подменного сервера EWS | `crates/backend-ews/tests/mock.rs` | `cargo test -p postman-backend-ews --test mock` |
| Разведка (только чтение) | `crates/ews/tests/probe.rs` | `cargo test -p postman-ews --test probe -- --ignored --nocapture` |
| Крейт против боевого (`#[ignore]`) | `crates/ews/tests/stand.rs` | `cargo test -p postman-ews --test stand -- --ignored --test-threads=1` |
| Учётка через плагин (`#[ignore]`) | `crates/backend-ews/tests/stand_exchange.rs` | `cargo test -p postman-backend-ews --test stand_exchange -- --ignored --test-threads=1` |

Последний прогон на боевом (2026-10-23, S7-07, S7-08): `postman-backend-ews` `stand_exchange` 7 из 7 (`sign_in`, `exchange_mail_round_trip`,
`exchange_calendar_and_people`, `exchange_meeting_cycle`, `exchange_series_invite`, `exchange_recurring_in_week`,
`exchange_directory_fields`); `postman-ews` `stand`: `sign_in`, `ews_event_round_trip`, `ews_recurring_series`, `ews_meeting_cycle`,
`ews_availability`, `ews_resolve_names`, `ews_find_people`. 2026-10-28 (T1-04а, T1-21а, T1-25а): `ews_mail_folder_ops`,
`ews_notify_new_mail`, `ews_contacts_round_trip`, `ews_directory_page`; плагин (T1-02а, T1-20, T1-21б, T1-25в):
`exchange_deleted_elsewhere`, `exchange_events_bring_mail`, `exchange_contacts_through_sync`.
| Руками | `docs/manual-tests/S7-06.md` | владелец |

Окружение — `docs/specs/S7.md` «Проверка на боевом Exchange» (`POSTMAN_EWS_*`); значения — только у владельца, вне
репозитория.

## Находки боевого Exchange (2026-10-23, тестовые учётки)

| № | Что | Итог |
|---|---|---|
| 1 | Версия | Exchange 2013 CU23 (15.0.1497.46), а не 2016+; `RequestServerVersion` `Exchange2013_SP1` принимает |
| 2 | IMAP4 и приём SMTP от клиентов | выключены на узле Exchange (открыт только 25); `mail.<домен>` — другая почтовая система → почта Exchange по EWS (решение владельца) |
| 3 | TLS | rustls не договаривается: сервер берёт ECDHE только с AES-CBC, а GCM — только без ECDHE → TLS EWS — SChannel Windows (`native-tls`) |
| 4 | Autodiscover POX | отвечает на `autodiscover.<домен>` (корень домена — 404); URL EWS — на другом узле домена |
| 5 | Вход | Negotiate (Kerberos или NTLM внутри) — у обеих тестовых учёток; держит ли IIS вход Kerberos на соединении — [проверить] |
| 6 | `GetItem` с `message:InternetMessageId`/`InReplyTo`/`References` | `ErrorInternalServerError` → список — `Default` + `item:Flag`, `Message-ID` и `References` — из MIME |
| 7 | `GetFolder` с `archive` в пачке стандартных папок | роняет весь запрос → «Архив» не запрашиваем |
| 8 | `SyncFolderHierarchy` | 2026-10-23 — `ErrorInternalServerError` (и под `msgfolderroot`) → дерево `FindFolder` (Deep); 2026-10-28 — отвечает (под `msgfolderroot`, с `folder:ParentFolderId` и `folder:FolderClass`), `FindFolder` — запасной путь |
| 9 | `DateTimeReceived` | сразу после отправки себе бывает пуст → дата из `DateTimeSent`/`DateTimeCreated` |
| 10 | `GetUserAvailability` с одинаковыми `StandardTime`/`DaylightTime` | падает внутри (`-2146233088`) → переходы UTC — в разные месяцы |
| 11 | UID встречи EWS = UID iCalendar приглашения из письма (ответ из карточки письма) | да: UID `text/calendar` письма = UID встречи у участника и у организатора (`exchange_meeting_cycle`, 2026-10-23) |
| 12 | Адреса участников — SMTP или `EX`; у участника после отмены — `IsCancelled` | SMTP (у тестовых учёток); после отмены у участника — `IsCancelled`, Postman её не показывает (`ews_meeting_cycle`, `exchange_meeting_cycle`, 2026-10-23) |
| 13 | Письма встреч в `GetItem` — элементы `MeetingRequest`, `MeetingCancellation`…, не `Message` | до S7-07 их не было в списке писем (приглашение не доходило до карточки) → разбор берёт все письма встреч; «прочитано» — тем же `UpdateItem` с `<t:Message>` (проходит) |
| 14 | `FindPeople` по GAL (2013 CU23) | работает; свойства `Title`, `Departments`, `CompanyName`, `OfficeLocations`, `PersonaType` — да; `Department`, `Titles`, `BusinessPhoneNumbers`, `MobilePhones`, `PhoneNumber` — `ErrorInvalidArgument` на весь запрос, `PhoneNumbers` — `ErrorInternalServerError` → телефона из `FindPeople` нет. Должность и отдел у коллег — [проверить] руками (у тестовых учёток пусто) |
| 15 | Серия без `FirstDayOfWeek` | сервер ставит воскресенье (`WKST=SU` в правиле) |
| 16 | Ответ на приглашение серии по экземпляру | отвечает только экземпляру → ответ — по серии (`RecurringMasterItemId`), `exchange_series_invite` |
| 17 | Уведомления: `Subscribe` streaming и pull, `GetStreamingEvents`, `GetEvents` (2026-10-28) | работают на 2013 CU23: письмо себе — событие через 3–9 с (`ews_notify_new_mail`, `exchange_events_bring_mail`) |
| 18 | `FindPeople` по GAL без `QueryString` (список страницами) | `ErrorInvalidOperation` → справочник только поиском (`ews_directory_page`) |
| 19 | Поиск контакта по `contacts:DisplayName` (`Contains` в `FindItem`) | ничего не находит → поиск своих контактов и уборка тестов — по `contacts:FileAs` (Postman пишет туда отображаемое имя); тема у контакта EWS пустая |
