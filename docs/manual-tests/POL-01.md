# POL-01. Политики: один набор ключей и типов

Windows (release-сборка, журнал в файл):
1. `reg add HKCU\Software\Policies\Pets\Postman /v StorePasswords /t REG_DWORD /d 0` — после перезапуска в
   «Безопасности» хранение пароля заперто («Задано организацией»). То же строкой `/d 0` (REG_SZ).
2. `... /v AllowUpdate /t REG_DWORD /d 0` — выключатель обновлений заперт, запросов нет.
3. `... /v StorePasswords /t REG_DWORD /d 5` — не заперто, в журнале WARN `policy: value of unexpected type`.
4. Только прежний `HKCU\Software\Policies\Aahz\Postman` `AllowUpdate` = 0 (DWORD) — заперто, в журнале WARN
   `AllowUpdate in legacy key`.

macOS (профиль конфигурации для пакета Postman):
5. `MinTls` `<real>1.3</real>` — TLS 1.3 закреплён; `<string>1.3</string>` — так же.
6. `StorePasswords` `<false/>` или `<integer>0</integer>` — заперто; `<data>` — WARN, не заперто.
