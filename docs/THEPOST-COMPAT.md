# Postman на The Post: совместимость

Проверка 2026-11-05: бета `beta-20261105`, эталон SMB в compose, Postman release 0.1.0. Данные — тестовые, `example.test`.

| Что | Итог |
|---|---|
| Сессия JMAP, вход по паролю приложения (Basic) | работает |
| Приём по SMTP, спам-фильтр (без SPF/DKIM — в «Спам») | работает |
| Папки, письма, непрочитанные, чтение, перенос между папками, синхронизация | работает |
| «Нет у сервера — нет на экране»: календарь, контакты, задачи скрыты | работает |
| Отправка из Postman: `EmailSubmission` принят, письмо в очереди | доставка локальному адресату идёт через DNS: на стенде без MX не дошло |
| `EmailSubmission/query` | сервер: `unknownMethod`; Postman вызывает его для «Отправить позже» и отмены |
| Календари, контакты, справочник, занятость, CalDAV | ещё нет на сервере (Ф2), `capabilities` их не объявляют |
| `Availability/get`, `CalendarPublication` | ещё нет на сервере (волна jmap-09), клиент готов (S8-45, S8-46) |

Стендовые тесты `postman-backend-jmap --test stand`: 7 из 19 прошли, 9 — ожидаемо без календарей, контактов и справочника, 3 — из-за
доставки после отправки (письмо не дошло, push о нём не пришёл). Находки по серверу — карточка CMP-01 в репозитории The Post.

## Как поднять стенд
1. `docker build` тонкого образа `thepost:dev` из архива беты (Debian и `ca-certificates`), пока образ не публикуется.
2. `docker compose -f deploy/compose/dev.yml --profile smb up -d --no-build`.
3. В своём конфиге стенда добавить слушатели `http.jmap`, `imap`, `smtp`, `smtp.delivery.local_domains`; создать
   `/var/lib/thepost/search`; в томе `tls` — `fullchain.pem`, `privkey.pem`.
4. Учётные записи: `domains`, `accounts`, `app_passwords` в PostgreSQL, папки — `Mailbox/set`: Admin API требует OIDC.
5. `POSTMAN_STAND_URL`, `POSTMAN_INSECURE_TLS=1`, `POSTMAN_STAND_USER1..3`, `POSTMAN_STAND_PASSWORD1..3`.
