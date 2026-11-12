# LIN-05: корпоративное на Linux — Kerberos, LDAP, S/MIME

lane: x
status: todo
deps: [LIN-03]
class: B
files: [crates/ews/src/, crates/backend-ews/, crates/backend-ldap/, crates/core/src/smime/, app-gpui/src/]
read: [docs/decisions/2026-11-12-linux.md, docs/tasks/MAC-05.md]
checks: [clippy, nextest, ci-linux]
manual: docs/manual-tests/LIN-05.md

## Цель
Вход в домен и S/MIME на Linux работают без Windows-хранилищ.

## Что входит
- EWS: Kerberos через GSSAPI (билет из `kinit`/SSSD), NTLM — своей реализацией; привязка канала.
- LDAP: вход GSSAPI (`ldap3` с `gssapi` уже собирается с `libkrb5-dev`) — проверить на стенде, отказ — понятной строкой.
- S/MIME: личные сертификаты и ключи — файл PKCS#12 с паролем из Secret Service; доверенные корни — системное
  хранилище (`/etc/ssl/certs`). Владелец 2026-11-12: ключи у сотрудников в файлах — токены
  (PKCS#11) и хранилища (OpenBao и др.) не делаем; ГОСТ на Linux — позже, LIN-07.
- Что недоступно на Linux — скрыто в настройках, а не падает.

## Приёмка
- `linux.yml` зелёный; ручная проверка `docs/manual-tests/LIN-05.md` на стенде с доменом — когда появится.

## Зависимости
LIN-03.
