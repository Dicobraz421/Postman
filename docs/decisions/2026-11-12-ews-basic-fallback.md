# EWS: Basic — запасной вход по TLS

Владелец, 2026-11-12 (MAC-05, PR #585). Исключение из ADR 0003 («EWS — только Negotiate/NTLM, Basic нет»).

- Basic включается только по TLS (`https`) и только если сервер в `WWW-Authenticate` не предлагает ни Negotiate, ни
  NTLM. Если предлагает — вход по-прежнему Negotiate или NTLM, Basic не пробуется.
- Лишних попыток входа нет: Basic — одна попытка вместо ошибки «сервер не предлагает Negotiate и NTLM», не повтор
  после отказа NTLM (счётчик неверных входов AD не растёт).
- Basic не пишется в журнал: ни заголовок, ни сам факт входа этой схемой.
- Где: `crates/ews/src/http.rs` (`Http::basic`), заголовок — `crates/ews/src/auth.rs` (`basic`).
