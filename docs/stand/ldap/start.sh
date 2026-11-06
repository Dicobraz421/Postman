#!/bin/sh
# Запуск каталога: конфигурация и данные собираются здесь, пароли — из окружения контейнера. В `/stand` смонтированы
# сертификат сервера и LDIF со стороны стенда (`docs/stand/net.py`).
set -eu

ROOT_HASH=$(slappasswd -s "$LDAP_ADMIN_PASSWORD")
READER_HASH=$(slappasswd -s "$LDAP_READER_PASSWORD")

mkdir -p /var/lib/openldap/run /var/lib/openldap/example
cat > /etc/openldap/slapd.conf <<CONF
include /etc/openldap/schema/core.schema
include /etc/openldap/schema/cosine.schema
include /etc/openldap/schema/inetorgperson.schema
modulepath /usr/lib/openldap
moduleload back_mdb
pidfile /var/lib/openldap/run/slapd.pid
argsfile /var/lib/openldap/run/slapd.args
TLSCertificateFile /stand/ldap.pem
TLSCertificateKeyFile /stand/ldap.key
database mdb
suffix "dc=example,dc=test"
rootdn "cn=admin,dc=example,dc=test"
rootpw $ROOT_HASH
directory /var/lib/openldap/example
index objectClass,mail eq
access to attrs=userPassword by anonymous auth by * none
access to * by * read
CONF

rm -f /var/lib/openldap/example/*.mdb
sed "s|@READER_HASH@|$READER_HASH|" /stand/data.ldif | slapadd -f /etc/openldap/slapd.conf -b dc=example,dc=test
exec slapd -d stats -f /etc/openldap/slapd.conf -h "ldap:/// ldaps:///"
