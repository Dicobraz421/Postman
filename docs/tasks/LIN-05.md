# LIN-05: корпоративное на Linux — Kerberos, LDAP, S/MIME

lane: x
status: review
deps: [LIN-03]
class: B
files: [crates/ews/src/, crates/backend-ews/, crates/backend-ldap/, crates/core/src/smime/, crates/smime/src/p12.rs, crates/smime/src/p12/, crates/smime/src/soft.rs, crates/smime/src/lib.rs]
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

## Сделано (ветка `x/lin-05`)
- EWS без пароля: GSS-API MIT Kerberos (`libgssapi_krb5`, тот же код, что GSS.framework на macOS, `crates/ews/src/sys.rs`)
  — SPNEGO с билетом из кэша по умолчанию, привязка к каналу TLS; NTLM через GSS — только при gss-ntlmssp. Вход с
  паролем — NTLMv2 на Rust (`ntlm.rs`), как было. `backend-ews` от ОС не зависит — без правок.
- LDAP: вход учётной записью — `sasl_gssapi_bind` `ldap3` (привязка к каналу TLS — его), отказ — строкой «Kerberos
  (GSSAPI) login failed — no valid domain ticket? …»; контроллер домена — SRV `_ldap._tcp.dc._msdcs.<область>` через
  `res_query` glibc (`sys/linux.rs`), разбор ответа DNS со сжатием — `srv::answers`.
- S/MIME: PKCS#12 — свой разбор в `postman-smime` (`p12.rs`: MAC SHA-1/SHA-2, PBES2 AES/3DES, старые 3DES и RC2 —
  крейт `rc2`, волна `x/lin-05c`); ключи RSA и EC P-256/P-384, как у Windows (P-521 — нет: крейта `p521` в дереве нет);
  подпись (RSA, ECDSA) и снятие ключа (RSA, ECDH) — ключом в памяти (`soft.rs`), шифрование получателям EC —
  эфемерный ECDH `p256`/`p384`. Ядро (`smime/linux.rs`):
  якоря — набор корней дистрибутива (`SSL_CERT_FILE`, `/etc/pki/…/email-ca-bundle.pem`, `/etc/ssl/certs/…`), «корпоративные» —
  добавленные администратором (`/usr/local/share/ca-certificates`, `…/ca-trust/source/anchors`), запрещённые —
  `blocklist`; свои ключи — `load_personal_files` (путь + пароль от приложения), `check_personal_file` для настроек;
  случайные числа — `/dev/urandom`.
- Отзыв (решение 2026-10-22 «проверка отзыва — всегда»), как CryptoAPI: сначала OCSP по AIA (`x/lin-05d`: запрос с
  nonce — ядро, `postman_smime::ocsp`; ответ — воркер, `SmimeOcsp`: подпись издателя или делегата с EKU
  `OCSPSigning`, `CertID`, nonce, срок, статус), затем CRL по CDP тем же путём, что ГОСТ без СКЗИ (`crl.rs`). Сеть —
  `smime/linux.rs`: HTTP и HTTPS (rustls с проверкой сертификата системы), перенаправления, `http(s)_proxy`/`no_proxy`.
  Итог — «отозван» / «не отозван» / «не удалось проверить» (`Unknown`, причина — в журнале).
- PAR-AUDIT (волна `x/lin-05c`): №5 — итог цепочки одним правилом на всех ОС (`smime::chain_verdict`, таблица
  `chain_verdicts_are_the_same_everywhere`): любая ошибка цепочки, кроме отзыва, — «отклонено», и вместе с «отзыв
  неизвестен» тоже (на Windows было `Unknown`). №6 — `seal::Ops::random` → `Result`: сбой генератора (Windows —
  код `BCryptGenRandom`, macOS — без нулевого буфера, Linux — `/dev/urandom`) останавливает шифрование
  (`random_failure_stops_sealing`). №7 (KeyUsage) — уже общий: подписанта проверяет воркер
  (`chain.rs`, `usage_allows_signing`) на всех ОС до проверки ОС.
- Что требует UI (выбор файла, пароль в Secret Service, скрытие недоступного, `upn`/`in_domain`) — `LIN-05b`.

## Приёмка
- `linux.yml` зелёный; ручная проверка `docs/manual-tests/LIN-05.md` на стенде с доменом — когда появится.

## Зависимости
LIN-03.
