# ADR 0003. Второй путь к серверу: стандарты (IMAP, SMTP, CalDAV, CardDAV) и минимальный EWS

Статус: принято (2026-10-23, владелец: модульное ядро остаётся). Меняет межкрейтовые контракты (`core` ↔ `jmap`,
`core` ↔ `app-gpui`). Спека сессии — `docs/specs/S7.md`. Уточнён ADR 0004 (2026-10-23): адаптеры — не в `core`, а
плагины `backend-*` (L2b); перечисления путей в `core` заменены договором плагинов — разделы «Слои» и «Нейтральная
модель» ниже читать с этой поправкой.

> Уточнение 2026-10-23 (вечер, `S0-decisions.md`): почта Exchange идёт **по EWS**, не через его IMAP/SMTP — у
> Exchange компании IMAP4 и приём SMTP от клиентов выключены (`docs/EXCHANGE.md`). Строки таблицы ниже «Почта,
> отправка → IMAP + SMTP submission Exchange» и абзац «Почему так у Exchange» в этой части заменены: почта
> Exchange — крейт `ews` (`SyncFolderHierarchy`/`SyncFolderItems`/`GetItem` MimeContent/`CreateItem` и т. д.).

⚠ отступление от ТЗ: `docs/HANDOFF.md` §1 — «JMAP-only». Решение владельца 2026-10-23 (`S0-decisions.md`): цель —
по-прежнему Stalwart по JMAP; второй путь — стандарты, на переходный период — ещё и Exchange компании (on-prem) в
том, чего стандартами не достать.

## Как сейчас

- `core` зависит от `jmap` в 10 модулях, больше всего — `sync.rs` (61 место): цикл синхронизации написан под JMAP.
- Типы JMAP — в публичном API `core` и интерфейса (`Filter`, `Release`, `Identity`, `Envelope`, `ContactCard`,
  `Principal`, `BusyPeriod`, `Auth` …); `app-gpui` зависит от `postman-jmap` напрямую (6 файлов).
- Кэш (redb) уже нейтрален: строки писем, папок, аккаунтов; ключи `scoped(аккаунт, id)`; состояние синхронизации —
  непрозрачная строка по ключу (`sync_state`).

## Решение

### Три вида учётной записи, у каждой области — свой путь

| Область | Stalwart (JMAP) | Стандарты | Exchange (on-prem) |
|---|---|---|---|
| Почта, отправка | JMAP | IMAP + SMTP submission | IMAP + SMTP submission Exchange |
| Календарь, приглашения, задачи | JMAP | CalDAV + scheduling (RFC 6638) | EWS: календарь и ответы на встречи; задач нет |
| Занятость | JMAP | RFC 6638 free/busy (POST в outbox) | EWS `GetUserAvailability` |
| Контакты | JMAP | CardDAV | — |
| Справочник (GAL) | JMAP `Principal` | — | EWS `ResolveNames` |
| Правила, автоответ, квота, таблица категорий, общие ящики | JMAP (как сейчас) | — | — |

Почему так у Exchange: CalDAV и CardDAV в Exchange Server нет (ни в 2016, ни в 2019, ни в SE; сторонние шлюзы вроде
DavMail — не наш путь) — календарь, занятость и GAL только через EWS. Почта — через IMAP/SMTP самого Exchange: это
тот же адаптер, что и для стандартов, своего кода для почты Exchange нет вовсе (почта по EWS — синхронизация папок,
`MimeContent`, отправка — отдельный клиент того же объёма, что весь IMAP). Условие — на сервере включены IMAP4 и
SMTP-приём от клиентов (S7-06 проверяет).

### Слои

| Слой | Крейт | Что |
|---|---|---|
| L1 | `jmap` (есть) | без изменений поведения |
| L1 | `imap` (новый) | IMAP (`async-imap` + `imap-proto`) и SMTP submission (`mail-send`) |
| L1 | `dav` (новый) | CalDAV и CardDAV: тонкий клиент WebDAV (PROPFIND, REPORT, PUT/DELETE с `If-Match`, POST в outbox) на `reqwest` (уже в сборке) и quick-xml |
| L1 | `ews` (новый) | минимальный EWS: SOAP, Autodiscover (POX), вход Negotiate/NTLM через SSPI Windows; только операции из таблицы S7 |
| L2 | `core` | нейтральная модель, адаптеры, общий цикл синхронизации |
| L3 | `app-gpui` | только типы `core`; **ни одного протокольного крейта** (тест `layers.rs`) |

iCalendar и vCard: разбор чужих данных — в воркере (`calcard`, уже там), сборка — `calendar`/`compose`, как для
iMIP сейчас. Ответы своего сервера (XML DAV и EWS, ответы IMAP) разбираются в L1 без DTD и сущностей, с пределами
размера; тела писем, .ics, .vcf — только воркер. `unsafe` (SSPI) — в `ews::sys`.

### Нейтральная модель — ровно под эти пути

- Типы из `jmap`, которые видят `core` и интерфейс, переезжают в `core::model` с теми же полями.
- Области — по одному перечислению на область, без общего «суперсервера»:
  `Mail { Jmap, Imap }`, `Calendar { Jmap, CalDav, Ews }`, `Contacts { Jmap, CardDav }`,
  `Directory { Jmap, Ews }`. Учётная запись = набор путей: Stalwart — всё `Jmap`; стандарты — `Imap` + `CalDav` +
  `CardDav`; Exchange — `Imap` + `Ews` (календарь, справочник). Перечисление вместо `dyn`: `async fn` в трейтах не
  dyn-совместимы, набор путей закрыт.
- Функции, которые есть только у JMAP (правила Sieve, автоответ, квота, таблица категорий FileNode, общие ящики,
  «Отправить позже» сервером), остаются кодом JMAP как есть — без абстракций; у других учёток их нет.
- `Features` учётной записи — какие области и функции есть; правило показа — одно, в `docs/specs/S7.md`
  («Нет у сервера — нет на экране»).

### Состояние, идентификаторы, push

- **Состояние** — `sync_state` кэша, ключ `<аккаунт>/<вид>[/<папка>]`, значение — строка адаптера: JMAP — `state`;
  IMAP — `UIDVALIDITY` + `HIGHESTMODSEQ` папки (без CONDSTORE — сверка UID и флагов окном); CalDAV/CardDAV —
  `sync-token` коллекции (без `sync-collection` — ETag ресурсов); EWS — `SyncState` папки «Календарь».
- **Идентификаторы.** JMAP — id сервера, как сейчас. У остальных ключ кэша — короткий локальный, таблица `remote`
  в кэше: IMAP — папка + `UIDVALIDITY` + UID; DAV — href + ETag; EWS — `ItemId` + `ChangeKey`. Связи между объектами
  (задача ↔ письмо, приглашение ↔ событие) — по `Message-ID` и UID iCalendar, как уже в `mailtask` и `invite`.
- **Push:** JMAP — EventSource; IMAP — IDLE на «Входящих»; CalDAV, CardDAV и EWS — опрос по таймеру синхронизации
  (как страховочный опрос JMAP сейчас, `sync::POLL`), без подписок.
- **Несколько учётных записей** — в одном потоке синхронизации, по задаче tokio на учётку; деревья папок — по
  учёткам, как общие ящики сейчас.
- **Секреты** — Credential Manager Windows, как сейчас. EWS — только Negotiate/NTLM через SSPI: на машине в домене —
  Kerberos от учётной записи Windows без пароля, вне домена — NTLM с паролем из Credential Manager (соединение
  HTTP/1.1 держится на всю сессию: NTLM привязан к TCP; привязку к каналу TLS Exchange 2019 CU14+ и SE включают по
  умолчанию — SSPI получает её из сертификата сервера).
  > Уточнения 2026-11-12 (MAC-05): вход с паролем — NTLMv2 на чистом Rust (`ews::ntlm`) на всех ОС, SSPI — только
  > без пароля. Исключение «Basic нет»: Basic — запасной, только по TLS и если сервер не предлагает ни Negotiate, ни
  > NTLM — `docs/decisions/2026-11-12-ews-basic-fallback.md`.

## Цена

- Переделка `sync.rs` и импорта типов в интерфейсе без изменения поведения JMAP: тесты стенда Stalwart зелёные до
  и после.
- Размер exe: `async-imap`, `imap-proto`, `mail-send`, quick-xml — плюс 1–2 МиБ [оценка] при запасе 0,67 МиБ
  (`docs/SIZE.md`); бюджет — по замеру S7-09.

## Откат

Новые пути — за признаком сборки (`features`) до приёмки S7-09: без них Postman собирается и работает, как до S7.
Нужно больше Exchange (почта по EWS, задачи, автоответ) — отдельным решением, когда понадобится.
