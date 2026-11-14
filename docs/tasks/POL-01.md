# POL-01: политики организации — один набор ключей и типов для всех ОС (PAR-AUDIT #3, #4, #12, #13)

lane: x
status: done
deps: []
class: B
branch: x/pol-01
files: [app-gpui/src/security.rs, app-gpui/src/update.rs, app-gpui/src/sys/]
read: [docs/tasks/PAR-AUDIT.md]
checks: [clippy, nextest]
manual: docs/manual-tests/POL-01.md

## Что сделать

- #3: политики читаются и `REG_SZ`, и `REG_DWORD` (`StorePasswords`/`AllowUpdate` = DWORD 0 действуют); неверный
  тип или значение — WARN в журнал, не применяется.
- #4: macOS — `MinTls` строкой и дробным числом (`<real>1.3</real>`) закрепляет TLS 1.3.
- #13: `AllowUpdate` — в общем ключе `Software\Policies\Pets\Postman`; прежний `Software\Policies\Aahz\Postman` —
  запасной, с WARN.
- #12 (macOS): проверка подписи и установка из одной копии — pets (`pets-update`), отдельный PR; пин здесь не трогать.

## Ключи и типы политик (Windows и macOS одинаково)

Windows — `Software\Policies\Pets\Postman` (HKLM, затем HKCU). macOS — принудительное значение профиля конфигурации
пакета Postman. Linux — политик нет.

| Ключ | Значения | Windows | macOS | Смысл |
|---|---|---|---|---|
| `ExternalImages` | `never` \| `ask` | `REG_SZ` | `<string>` | внешние картинки: никогда / спрашивать |
| `MinTls` | `1.2` \| `1.3` | `REG_SZ` | `<string>` или `<real>` | минимальная версия TLS |
| `StorePasswords` | `0` \| `1` | `REG_DWORD` или `REG_SZ` (`0/1`, `false/true`, `off/on`, `no/yes`) | `<integer>`, `<true/>`/`<false/>`, `<string>` | хранить пароль |
| `AllowUpdate` | `0` \| `1` | то же | то же | самообновление; прежний `Software\Policies\Aahz\Postman` (DWORD) — запасной, WARN |
| `AllowChannelTelegram` | `0` \| `1` | то же | то же | канал Telegram (ADR 0012 п. 11, CH-03): `0` — канала нет в настройках, учётки не подключаются; `1` — включён, выключатель недоступен |

Прочее (другой тип, число вне `0/1`, `REG_BINARY`, массив) — WARN `policy: value of unexpected type or range, not
applied`, значение не закреплено. Код: `security::policy_flag`/`policy_tls13`/`policy_images`, чтение —
`sys::shell::policy` (Windows `RegGetValueW` `RRF_RT_ANY`, macOS `CFPreferences`).

## Решения для владельца

- Чтение значения любого типа сейчас в `app-gpui/src/sys/` (свой вызов ОС: Cargo.toml в этой волне не трогать,
  GKIT-02). При следующем подъёме пина pets — перенести в `pets_shell::registry`/`managed` (pets, класс A).
