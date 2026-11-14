# PAR-02b: «ещё не действует» и промежуточные из писем

lane: x
status: done
deps: [PAR-02]
class: B
files: [crates/core/src/smime/, crates/smime/src/lib.rs, crates/smime/src/chain.rs, crates/smime/tests/verify.rs, app-gpui/src/smime.rs, app-gpui/i18n/ru/smime.ftl, app-gpui/i18n/en/smime.ftl, docs/tasks/PAR-02b.md, docs/manual-tests/PAR-02.md]
read: [docs/adr/0011-platform-contract.md, docs/tasks/PAR-02.md]
checks: [clippy, nextest, clippy-windows, clippy-macos, ci-windows, ci-macos]
manual: docs/manual-tests/PAR-02.md

## Цель
Решения по «Решениям для владельца» PAR-02 (оркестратор 2026-11-13, владелец передал решение): (1) «ещё не действует» —
своя причина на всех ОС; (2) промежуточные для Linux и macOS — кэш из полученных подписанных писем; (3) замок
Связки ключей, сериализующий окна PIN, — принят как есть.

## Сделано (ветка `x/par-02b`)
- (1) Воркер: `Status::NotYetValid` (`CertNotValidYet` webpki) — тест в `tests/verify.rs`. Ядро: `Confirm::NotYetValid`,
  `ChainCheck::not_yet_valid` (после «истёк»); macOS — `errSecCertificateNotValidYet`; Windows — у флага
  `CERT_TRUST_IS_NOT_TIME_VALID` срок различает `CertVerifyTimeValidity` по элементам цепочки (истёк хоть один —
  «истёк»). Fluent ru/en: `smime-verdict-not-yet-valid`, `smime-reason-not-yet-valid`; «истёк» — без «или ещё не
  начался». Минимальная правка `app-gpui/src/smime.rs` (две ветки `match`) — по поручению, в чужой полосе.
- (2) `smime/seen.rs`: промежуточные доверенной цепочки (без подписанта и корня) — в `smime-intermediates.bin` в
  каталоге данных (0600 на Unix, атомарная запись), ≤ 256 штук, ≤ 16 КиБ каждый, ≤ 1 МиБ всего, старые вытесняются.
  Только для построения цепочки: в `Trust::intermediates` (в пределе 7/8 `MAX_TRUST`), корни и запреты — из системы.
  Ядро байты не разбирает. macOS — вместе с сертификатами Связки ключей (PAR-02); Windows — хранилища `CA`, кэш не
  нужен (`// платформа:`).

## Договор (ADR 0011)

| функция | отказ | журнал | пределы и права | на диске | политики | Fluent | отличие ОС и причина |
|---|---|---|---|---|---|---|---|
| «Ещё не действует» | `Rejected(NotYetValid)` / `Status::NotYetValid` — значок «сертификат не годится» | — | — | — | — | `smime-verdict-not-yet-valid`, `smime-reason-not-yet-valid` | Windows: один флаг на оба случая — срок различается по элементам цепочки |
| Промежуточные из писем | файл не читается/не пишется — только память; WARN при записи | WARN `smime: intermediate certificates not saved: <kind>` | 256 шт., 16 КиБ, 1 МиБ; только владелец | `smime-intermediates.bin` в каталоге данных | — | — | Windows: хранилища `CA` ОС вместо кэша |

## Приёмка
clippy Linux / `x86_64-pc-windows-msvc` / `aarch64-apple-darwin` (C-зависимости заглушкой); nextest smime ядра
(65), `postman-smime --test verify` (14), `postman-worker --test smime` (7) на Linux. Ручная — `docs/manual-tests/PAR-02.md`.
