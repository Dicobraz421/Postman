# Сертификаты получателей из каталога — пока Active Directory (№12 SMIME.md)

Владелец, 2026-11-02: «пока бери из имеющегося АД». Выбор IdP — позже; тогда его каталог добавится вторым источником.

Для Ф5 (S5-12.5а, S5-12.5б):
- Каталог — Active Directory домена, в который входит компьютер пользователя: контроллер домена — службой Windows
  (`DsGetDcName`), base DN — `defaultNamingContext` из rootDSE. Сервер и base DN вручную — для компьютера вне домена.
- Вход — учётной записью Windows (Kerberos/SSPI), без пароля; связь — LDAPS или LDAP с подписью и шифрованием SASL.
- Поиск — по `mail` и `proxyAddresses` (`smtp:`), атрибуты `userCertificate;binary` и `userSMIMECertificate`.
- Стенд — AD тестового Exchange, только тестовые учётные записи.
