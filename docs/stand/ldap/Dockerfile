# Каталог стенда для S5-12.5: OpenLDAP с TLS. Как поднять — `docs/DEV.md`, раздел «Стенд сети».
FROM alpine:latest
RUN apk add --no-cache openldap openldap-back-mdb openldap-clients openldap-overlay-all
COPY start.sh /start.sh
RUN tr -d '\r' < /start.sh > /tmp/start.sh && mv /tmp/start.sh /start.sh && chmod +x /start.sh
EXPOSE 389 636
ENTRYPOINT ["/start.sh"]
