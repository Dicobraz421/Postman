# MAC-05: корпоративное на macOS — Exchange, LDAP, S/MIME

lane: x
status: todo
deps: [MAC-02]
class: B
files: [crates/ews/src/sys.rs, crates/ews/src/auth.rs, crates/backend-ldap/src/sys.rs, crates/core/src/smime/]
read: [docs/EXCHANGE.md, docs/SMIME.md, docs/decisions/2026-11-11-macos.md]
checks: [clippy, nextest]
manual: docs/manual-tests/MAC-05.md

## Цель
Корпоративный ящик на Mac: вход в Exchange без пароля в домене, сертификаты адресатов из каталога, подпись и
шифрование S/MIME ключами из Связки ключей.

## Что входит
- EWS: NTLM / Kerberos вместо SSPI (`crates/ews/src/sys.rs`): крейт `sspi` (Devolutions) или GSS.framework; привязка
  к каналу TLS (`sec_channel_bindings` — сейчас только Windows и тесты).
- LDAP: GSSAPI-привязка `ldap3`, контроллер домена — DNS SRV вместо `DsGetDcNameW` (без домена — нет).
- S/MIME: доверенные корни — `SecTrust`, подпись и расшифровка — `SecIdentity`/`SecKey` из Связки ключей (сейчас
  `smime/other.rs`); CRL — уже чистый Rust. Проверка подписи в воркере от платформы не зависит.
- ГОСТ при СКЗИ не делается (отложено, `docs/decisions/2026-10-27-smime-gost-skzi-deferred.md`).

## Приёмка
- `docs/manual-tests/MAC-05.md` на M1 с корпоративным ящиком владельца: вход EWS, поиск сертификата в каталоге,
  подписанное и зашифрованное письмо туда и обратно с Outlook для Windows.

## Зависимости
MAC-02 (Связка ключей).
