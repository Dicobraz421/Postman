# MAC-05: корпоративное на macOS — Exchange, LDAP, S/MIME

lane: x
status: todo
deps: [MAC-02]
class: B
files: [crates/ews/src/sys.rs, crates/ews/src/auth.rs, crates/backend-ldap/Cargo.toml, crates/backend-ldap/src/, crates/core/src/smime/]
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
  Сейчас (MAC-01) на macOS `ldap3` собран без фичи `gssapi`: `libgssapi` 0.9 не собирается с GSS.framework (нет
  `gss_wrap_iov`, `gss_unwrap_iov`, `gss_wrap_iov_length`), поэтому в `crates/backend-ldap/Cargo.toml` фича
  `gssapi` — только `cfg(not(target_os = "macos"))`, а вход «учётной записью» (`Login::Windows`) на macOS отвечает
  `DirectoryError::Unavailable` («Kerberos (GSSAPI) login … is not supported on macOS yet»). Простой вход по TLS и
  анонимный — работают. Здесь: GSSAPI через GSS.framework (свой SASL GSSAPI поверх `sasl_*`-вызовов `ldap3` или
  `cross-krb5`/`libgssapi` с поддержкой macOS), снять `cfg`.
  ✅ Сделано (ветка `x/mac-05-ldap`): свой SASL GSSAPI (RFC 4752, `crates/backend-ldap/src/kerberos.rs`) поверх
  публичного GSS-API GSS.framework (`src/sys/macos.rs`) — у `ldap3` нет общего SASL-вызова, а `iov` у `cross-krb5`
  включён по умолчанию (в 0.5 и `libgssapi` 0.11 — тоже). Соединение, StartTLS/TLS с привязкой к каналу и обмен —
  свои, `ldap3` получает готовое соединение через `ldapi:///` (мост: как есть по TLS, со слоем шифрования по
  `ldap://`). `cfg` у фичи `gssapi` остался (на macOS она не собирается), отказ `Unavailable` снят. Контроллер домена
  — SRV `_ldap._tcp.dc._msdcs.<домен>` через mDNSResponder, домен — область билета Kerberos; без билета — нет.
  Тесты без сервера — обмен на подменном GSS, мост, привязка к каналу, разбор SRV. Ручная проверка —
  `docs/manual-tests/MAC-05.md`, «Каталог LDAP».
- S/MIME: доверенные корни — `SecTrust`, подпись и расшифровка — `SecIdentity`/`SecKey` из Связки ключей (сейчас
  `smime/other.rs`); CRL — уже чистый Rust. Проверка подписи в воркере от платформы не зависит.
- Вход без пароля в Exchange: `sys::shell::upn` и `in_domain` на macOS (`app-gpui/src/sys/shell_macos.rs`) — сейчас
  «нет» (MAC-04), чтобы не предлагать вход, который не работает; здесь — домен AD (`dsconfigad -show` или Kerberos
  `klist`) и UPN.
- Тексты корпоративного уже без названий ОС (MAC-04, решение 2026-11-12 «no-os-names»: «проверяет система»,
  «учётная запись домена»); если на macOS проверка устроена иначе — поправить смысл, не добавляя названий ОС.
- ГОСТ при СКЗИ не делается (отложено, `docs/decisions/2026-10-27-smime-gost-skzi-deferred.md`).

## Приёмка
- `docs/manual-tests/MAC-05.md` на M1 с корпоративным ящиком владельца: вход EWS, поиск сертификата в каталоге,
  подписанное и зашифрованное письмо туда и обратно с Outlook для Windows.

## Зависимости
MAC-02 (Связка ключей).
