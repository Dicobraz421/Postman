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
- [x] EWS, вход с паролем на обеих ОС (PR «MAC-05: вход в Exchange с паролем»): NTLMv2 на чистом Rust
  (`crates/ews/src/ntlm.rs`) вместо SSPI, с привязкой к каналу TLS и MIC; «Имя пользователя (если отличается от
  адреса)» в мастере; вторая попытка `ДОМЕН\имя` из CHALLENGE и попытка без привязки; Basic — запасной по TLS;
  подробности отказа в тексте ошибки. Крейт `sspi` не подошёл: закреплённый `rsa =0.10.0-rc.18` против
  `=0.10.0-rc.19` у `postman-smime`.
- [x] EWS без пароля на macOS (PR «MAC-05: вход в Exchange без пароля на macOS»): GSS.framework (`sys.rs`) —
  SPNEGO или NTLM с учётными данными системы по умолчанию (билет Kerberos домена AD, Kerberos SSO, `kinit`),
  привязка к каналу TLS — `gss_channel_bindings_struct`; `upn` (`klist`, иначе `dsconfigad -show` и имя входа) и
  `in_domain` (действующий билет, `klist --test`) в `shell_macos.rs`. Тест `windows_sign_in_without_password` — и на
  macOS: учётные данные NTLM в хранилище системы (`postman_ews::SystemLogin`, `gss_aapl_initial_cred`). Тексты входа
  без названий ОС — MAC-04.
- LDAP: GSSAPI-привязка `ldap3`, контроллер домена — DNS SRV вместо `DsGetDcNameW` (без домена — нет).
  Сейчас (MAC-01) на macOS `ldap3` собран без фичи `gssapi`: `libgssapi` 0.9 не собирается с GSS.framework (нет
  `gss_wrap_iov`, `gss_unwrap_iov`, `gss_wrap_iov_length`), поэтому в `crates/backend-ldap/Cargo.toml` фича
  `gssapi` — только `cfg(not(target_os = "macos"))`, а вход «учётной записью» (`Login::Windows`) на macOS отвечает
  `DirectoryError::Unavailable` («Kerberos (GSSAPI) login … is not supported on macOS yet»). Простой вход по TLS и
  анонимный — работают. Здесь: GSSAPI через GSS.framework (свой SASL GSSAPI поверх `sasl_*`-вызовов `ldap3` или
  `cross-krb5`/`libgssapi` с поддержкой macOS), снять `cfg`.
- ✅ S/MIME (ветка `x/mac-05-smime`): доверенные корни — `SecTrust`, подпись и расшифровка — `SecIdentity`/`SecKey`
  из Связки ключей; CRL — уже чистый Rust. Проверка подписи в воркере от платформы не зависит. Сделано:
  `smime/macos.rs` (якоря: корни системы и настройки доверия, «корпоративные» — домен администратора; «Никогда не
  доверять» — запрещённые; подтверждение цепочки с отзывом), `smime/keychain.rs` (удостоверения Связки ключей и
  смарт-карт, RSA PKCS#1/OAEP, ECDH, подпись RSA/ECDSA, шифрование исходящих, сертификаты адресатов из Связки),
  `personal_certificates()` — список своих сертификатов для настроек (Windows — `My`); тест во временной Связке
  ключей CI — подпись и расшифровка туда и обратно. Подсказка «Проверка отзыва» — без «до 10 с» (срок — у системы).
  Описание — `docs/SMIME.md` §4а, ручная проверка — `docs/manual-tests/MAC-05.md`, раздел «S/MIME».
  Решения по вопросам PR — `docs/decisions/2026-11-12-macos-smime.md`; экран своих сертификатов — `S5-12.7`.
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
