"""Вторая сторона сцен `app-gpui/src/drill_net.rs`: источники календарей по https и каталог OpenLDAP (`docs/DEV.md`, «Стенд сети»).

  python net.py certs                               # сертификаты, LDIF и пароли стенда в каталоге данных
  python net.py ldap                                # контейнер `postman-stand-ldap`: собрать образ и запустить
  python net.py run <postman.exe> <снимки> <данные> [группа …]   # сцены; без имён групп — все
  python net.py serve                               # только https и JMAP: посмотреть руками

Все данные — `example.test`. Каталог стенда — `POSTMAN_NET_DIR`, по умолчанию `%LOCALAPPDATA%\\Pets\\testdata\\postman\\net`, вне
репозитория: там ключи и пароли. Пароли — из окружения (`POSTMAN_NET_LDAP_ADMIN`, `POSTMAN_NET_LDAP_READER`, `POSTMAN_NET_MAIL`), иначе
из файла `secrets.json` этого каталога (создаётся командой `certs`); в вывод не попадают.

Источники https — один процесс на 127.0.0.1: Postman идёт к ним через `HTTPS_PROXY` (CONNECT). Так ссылка остаётся настоящей
(`https://cal.example.test/…`: IP и `localhost` продукт не принимает, имена Windows не трогаем), имя сервера и цепочку клиент проверяет
как обычно. Корень стенда `POSTMAN_TEST_CA` читает только отладочная сборка (`cfg(debug_assertions)`). Зависимость — `cryptography`.
"""

import base64
import datetime
import hashlib
import http.server
import json
import os
import re
import secrets
import socket
import socketserver
import ssl
import subprocess
import sys
import threading
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
HOME = os.environ.get("POSTMAN_NET_DIR") or os.path.join(os.environ["LOCALAPPDATA"], "Pets", "testdata", "postman", "net")
CONTAINER = "postman-stand-ldap"
IMAGE = "postman-stand-ldap:local"
LDAP_PORT, LDAPS_PORT, SILENT_PORT = 13890, 16360, 13891
ME = "alice@example.test"
CAL = "cal.example.test"
GUEST, BOOKED = "guest@partner.example.test", "booked@partner.example.test"


def path(name):
    return os.path.join(HOME, name)


# --- сертификаты ---


def certs():
    """Два корня (`ca` — доверенный стендом, `other-ca` — чужой), серверы и сертификаты адресатов S/MIME."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

    os.makedirs(HOME, exist_ok=True)
    now = datetime.datetime.now(datetime.timezone.utc)
    start, end = now - datetime.timedelta(days=1), now + datetime.timedelta(days=825)

    def key():
        return rsa.generate_private_key(public_exponent=65537, key_size=2048)

    def name(text):
        return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, text), x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Postman Stand")])

    def builder(subject, issuer, public):
        return (x509.CertificateBuilder().subject_name(subject).issuer_name(issuer).public_key(public).serial_number(x509.random_serial_number())
                .not_valid_before(start).not_valid_after(end))

    def root(text):
        k = key()
        cert = (builder(name(text), name(text), k.public_key())
                .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
                .add_extension(x509.KeyUsage(False, False, False, False, False, True, True, False, False), critical=True)
                .add_extension(x509.SubjectKeyIdentifier.from_public_key(k.public_key()), critical=False)
                .sign(k, hashes.SHA256()))
        return cert, k

    def leaf(ca, text, alt, eku):
        k = key()
        use = x509.KeyUsage(True, False, True, False, False, False, False, False, False)
        cert = (builder(name(text), ca[0].subject, k.public_key())
                .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
                .add_extension(use, critical=True)
                .add_extension(x509.ExtendedKeyUsage([eku]), critical=False)
                .add_extension(x509.SubjectAlternativeName(alt), critical=False)
                .add_extension(x509.SubjectKeyIdentifier.from_public_key(k.public_key()), critical=False)
                .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(ca[1].public_key()), critical=False)
                .sign(ca[1], hashes.SHA256()))
        return cert, k

    def save(stem, pair):
        cert, k = pair
        with open(path(stem + ".pem"), "wb") as f:
            f.write(cert.public_bytes(serialization.Encoding.PEM))
        with open(path(stem + ".der"), "wb") as f:
            f.write(cert.public_bytes(serialization.Encoding.DER))
        with open(path(stem + ".key"), "wb") as f:
            f.write(k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.TraditionalOpenSSL, serialization.NoEncryption()))

    def server(issuer, *names):
        return leaf(issuer, names[0], [x509.DNSName(n) for n in names], ExtendedKeyUsageOID.SERVER_AUTH)

    def person(issuer, who):
        return leaf(issuer, who, [x509.RFC822Name(f"{who}@example.test")], ExtendedKeyUsageOID.EMAIL_PROTECTION)

    ca, other = root("Postman Stand CA"), root("Postman Stand Other CA")
    save("ca", ca)
    save("other-ca", other)
    save("cal", server(ca, CAL))
    save("untrusted", server(other, "untrusted.example.test"))
    save("ldap", server(ca, "localhost", "ldap.example.test"))
    for who, issuer in (("alice", ca), ("bob", ca), ("carol", other)):
        save(who, person(issuer, who))
    ldif()
    file = path("secrets.json")
    if not os.path.exists(file):
        with open(file, "w", encoding="utf-8") as f:
            json.dump({"admin": secrets.token_urlsafe(24), "reader": secrets.token_urlsafe(24), "mail": secrets.token_urlsafe(24)}, f)
    print("сертификаты и данные:", HOME)


def ldif():
    """Данные каталога: адрес с сертификатом стенда, с чужим, без сертификата и служебная запись."""

    def der(stem):
        with open(path(stem + ".der"), "rb") as f:
            return base64.b64encode(f.read()).decode()

    def person(uid, cert=None):
        lines = [f"dn: uid={uid},ou=people,dc=example,dc=test", "objectClass: inetOrgPerson", f"uid: {uid}", f"cn: {uid}", f"sn: {uid}", f"mail: {uid}@example.test"]
        if cert:
            lines.append(f"userCertificate;binary:: {der(cert)}")
        return "\n".join(lines)

    parts = ["dn: dc=example,dc=test\nobjectClass: dcObject\nobjectClass: organization\ndc: example\no: Example Test",
             "dn: ou=people,dc=example,dc=test\nobjectClass: organizationalUnit\nou: people",
             "dn: cn=reader,dc=example,dc=test\nobjectClass: organizationalRole\nobjectClass: simpleSecurityObject\ncn: reader\nuserPassword: @READER_HASH@",
             person("alice", "alice"), person("bob", "bob"), person("carol", "carol"), person("dave")]
    with open(path("data.ldif"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n\n".join(parts) + "\n")


def secret(name, env):
    if os.environ.get(env):
        return os.environ[env]
    with open(path("secrets.json"), encoding="utf-8") as f:
        return json.load(f)[name]


def docker(*args, check=True):
    return subprocess.run(["docker", *args], check=check, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")


def stand_mount():
    """Каталог для контейнера: только сертификат сервера и LDIF — ключи адресатов и пароли внутрь не попадают."""
    mount = path("ldap-mount")
    os.makedirs(mount, exist_ok=True)
    for src, dst in (("ldap.pem", "ldap.pem"), ("ldap.key", "ldap.key"), ("data.ldif", "data.ldif")):
        with open(path(src), "rb") as f, open(os.path.join(mount, dst), "wb") as g:
            g.write(f.read())
    return mount


def slapd_ready():
    got = docker("exec", CONTAINER, "ldapsearch", "-x", "-H", "ldap://127.0.0.1", "-b", "", "-s", "base", "namingContexts", check=False)
    return "dc=example,dc=test" in got.stdout


def ldap_up():
    """Контейнер каталога: идущий заменяется (данные — из LDIF, своего состояния в нём нет)."""
    if not os.path.exists(path("data.ldif")):
        certs()
    docker("build", "-q", "-t", IMAGE, os.path.join(HERE, "ldap"))
    docker("rm", "-f", CONTAINER, check=False)
    env = dict(os.environ, LDAP_ADMIN_PASSWORD=secret("admin", "POSTMAN_NET_LDAP_ADMIN"), LDAP_READER_PASSWORD=secret("reader", "POSTMAN_NET_LDAP_READER"))
    subprocess.run(["docker", "run", "-d", "--name", CONTAINER, "-p", f"127.0.0.1:{LDAP_PORT}:389", "-p", f"127.0.0.1:{LDAPS_PORT}:636", "-e", "LDAP_ADMIN_PASSWORD",
                    "-e", "LDAP_READER_PASSWORD", "-v", f"{stand_mount()}:/stand:ro", IMAGE], check=True, env=env, stdout=subprocess.DEVNULL)
    for _ in range(40):
        if slapd_ready():
            print("каталог запущен:", CONTAINER)
            return
        time.sleep(0.5)
    raise AssertionError("каталог не поднялся: " + docker("logs", "--tail", "20", CONTAINER, check=False).stdout)


# --- источники https ---

ICS_ZONES = """BEGIN:VTIMEZONE
TZID:Asia/Tokyo
BEGIN:STANDARD
DTSTART:19700101T000000
TZOFFSETFROM:+0900
TZOFFSETTO:+0900
TZNAME:JST
END:STANDARD
END:VTIMEZONE
BEGIN:VTIMEZONE
TZID:America/New_York
BEGIN:STANDARD
DTSTART:19701101T020000
RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU
TZOFFSETFROM:-0400
TZOFFSETTO:-0500
TZNAME:EST
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:19700308T020000
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU
TZOFFSETFROM:-0500
TZOFFSETTO:-0400
TZNAME:EDT
END:DAYLIGHT
END:VTIMEZONE
BEGIN:VTIMEZONE
TZID:Russian Standard Time
BEGIN:STANDARD
DTSTART:16010101T000000
TZOFFSETFROM:+0300
TZOFFSETTO:+0300
END:STANDARD
END:VTIMEZONE
"""

# Повторы — с января: неделя прогона всегда внутри серии. Что ждать в неделе — `series` и `subscribed` в `drill_net.rs`.
# `;UNTIL@` заменяет `calendar`: конечные серии или бесконечные (`endless`, группа `endless`).
ICS_EVENTS = """BEGIN:VEVENT
UID:outlook@example.test
DTSTAMP:20260101T000000Z
DTSTART;TZID=Russian Standard Time:20260105T120000
DTEND;TZID=Russian Standard Time:20260105T130000
RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL@
SUMMARY:Пояс Outlook
END:VEVENT
BEGIN:VEVENT
UID:tokyo@example.test
DTSTAMP:20260101T000000Z
DTSTART;TZID=Asia/Tokyo:20260106T100000
DTEND;TZID=Asia/Tokyo:20260106T110000
RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL@
SUMMARY:Пояс Токио
END:VEVENT
BEGIN:VEVENT
UID:tentative@example.test
DTSTAMP:20260101T000000Z
DTSTART:20260106T150000Z
DTEND:20260106T160000Z
RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL@
STATUS:TENTATIVE
SUMMARY:Под вопросом
END:VEVENT
BEGIN:VEVENT
UID:transparent@example.test
DTSTAMP:20260101T000000Z
DTSTART:20260107T150000Z
DTEND:20260107T160000Z
RRULE:FREQ=WEEKLY;BYDAY=WE;UNTIL@
TRANSP:TRANSPARENT
SUMMARY:Прозрачное
END:VEVENT
BEGIN:VEVENT
UID:cancelled@example.test
DTSTAMP:20260101T000000Z
DTSTART:20260108T150000Z
DTEND:20260108T160000Z
RRULE:FREQ=WEEKLY;BYDAY=TH;UNTIL@
STATUS:CANCELLED
SUMMARY:Отменённое
END:VEVENT
BEGIN:VEVENT
UID:utc@example.test
DTSTAMP:20260101T000000Z
DTSTART:20260108T090000Z
DURATION:PT1H30M
RRULE:FREQ=WEEKLY;BYDAY=TH;UNTIL@
SUMMARY:Время UTC
END:VEVENT
BEGIN:VEVENT
UID:york@example.test
DTSTAMP:20260101T000000Z
DTSTART;TZID=America/New_York:20260109T090000
DTEND;TZID=America/New_York:20260109T100000
RRULE:FREQ=WEEKLY;BYDAY=FR;UNTIL@
SUMMARY:Пояс Нью-Йорк
END:VEVENT
"""

ICS_ALL_DAY = """BEGIN:VEVENT
UID:allday@example.test
DTSTAMP:20260101T000000Z
DTSTART;VALUE=DATE:20260107
DTEND;VALUE=DATE:20260108
RRULE:FREQ=WEEKLY;BYDAY=WE;UNTIL#
TRANSP:TRANSPARENT
SUMMARY:Весь день
END:VEVENT
"""

ICS_FRESH = """BEGIN:VEVENT
UID:fresh@example.test
DTSTAMP:20260101T000000Z
DTSTART:20260106T060000Z
DTEND:20260106T070000Z
RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL@
SUMMARY:После обновления
END:VEVENT
"""


def calendar(*parts, name=None, endless=False, utc_named=False):
    """`endless` — серии без конца; `utc_named` — `TZID=UTC` вместо `Z` (известный дефект: группа `defects`)."""
    head = "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//Postman Stand//RU\nCALSCALE:GREGORIAN\n"
    if name:
        head += f"X-WR-CALNAME:{name}\n"
    end = datetime.date.today() + datetime.timedelta(days=400)
    text = head + ICS_ZONES + "".join(parts) + "END:VCALENDAR\n"
    text = text.replace(";UNTIL@", "" if endless else f";UNTIL={end:%Y%m%d}T235959Z").replace(";UNTIL#", "" if endless else f";UNTIL={end:%Y%m%d}")
    if utc_named:
        text = re.sub(r"^(DTSTART|DTEND):(\d{8}T\d{6})Z$", r"\1;TZID=UTC:\2", text, flags=re.M)
    return text.replace("\n", "\r\n").encode()


def free_busy():
    """VFREEBUSY на шесть недель вокруг сегодня: на понедельник — занято, под вопросом, нет на месте и свободный интервал, каждый
    день — час в 11:00 по времени этой машины: он попадает в рабочие часы помощника. Что ждать — `vfb` в `drill_net.rs`."""
    today = datetime.datetime.now(datetime.timezone.utc).date()
    monday = today - datetime.timedelta(days=today.weekday())
    lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Postman Stand//RU", "BEGIN:VFREEBUSY", "DTSTAMP:20260101T000000Z", f"ORGANIZER:mailto:{GUEST}"]
    for week in range(-1, 5):
        first = monday + datetime.timedelta(days=7 * week)
        stamp = first.strftime("%Y%m%d")
        lines.append(f"FREEBUSY;FBTYPE=BUSY:{stamp}T050000Z/{stamp}T060000Z")
        lines.append(f"FREEBUSY;FBTYPE=BUSY-TENTATIVE:{stamp}T070000Z/PT1H")
        lines.append(f"FREEBUSY;FBTYPE=BUSY-UNAVAILABLE:{stamp}T150000Z/{stamp}T160000Z")
        lines.append(f"FREEBUSY;FBTYPE=FREE:{stamp}T170000Z/{stamp}T180000Z")
        for day in range(7):
            when = first + datetime.timedelta(days=day)
            local = datetime.datetime(when.year, when.month, when.day, 11).astimezone(datetime.timezone.utc)
            lines.append(f"FREEBUSY;FBTYPE=BUSY:{local:%Y%m%dT%H%M%S}Z/PT1H")
    lines += ["END:VFREEBUSY", "END:VCALENDAR", ""]
    return "\r\n".join(lines).encode()


class Net:
    """Состояние источников: журнал запросов, что сломано, какая версия файла."""

    def __init__(self):
        self.lock = threading.Lock()
        self.journal = []
        self.down = False
        self.fresh = False

    def note(self, host, method, target, tag, status):
        with self.lock:
            self.journal.append({"host": host, "method": method, "path": target, "tag": tag, "status": status})

    def requests(self, target):
        with self.lock:
            return [r for r in self.journal if r["path"] == target]

    def statuses(self):
        with self.lock:
            return [r["status"] for r in self.journal]

    def body(self, target):
        """Тело ресурса, задержка ответа, отдавать ли ETag."""
        if target == "/public/basic.ics":
            parts = [ICS_EVENTS, ICS_ALL_DAY] + ([ICS_FRESH] if self.fresh else [])
            return calendar(*parts, name="Стенд подписка", utc_named=True), 0, True
        if target == "/public/endless.ics":
            return calendar(ICS_EVENTS, name="Стенд бесконечные", endless=True, utc_named=True), 0, False
        if target == "/public/utc.ics":
            return calendar(ICS_EVENTS, name="Стенд UTC"), 0, False
        if target == "/public/second.ics":
            return calendar(ICS_ALL_DAY, name="Стенд второй"), 0, False
        if target == "/free-busy/guest.ics":
            return calendar(ICS_EVENTS), 1.0, False
        if target in ("/free-busy/guest.vfb", "/free-busy/booked.vfb"):
            return free_busy(), 0, False
        return None, 0, False

    def respond(self, conn, host):
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = conn.recv(65536)
            if not chunk:
                return
            data += chunk
        head = data.split(b"\r\n\r\n", 1)[0].decode("latin-1").split("\r\n")
        method, target = head[0].split(" ")[:2]
        headers = {k.strip().lower(): v.strip() for k, v in (line.split(":", 1) for line in head[1:] if ":" in line)}
        tag = headers.get("if-none-match")
        with self.lock:
            body, delay, with_tag = self.body(target)
        if body is None:
            self.note(host, method, target, tag, 404)
            conn.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        etag = '"' + hashlib.sha256(body).hexdigest()[:16] + '"'
        if with_tag and tag == etag:
            self.note(host, method, target, tag, 304)
            # Следующий запрос получит новую версию: сцены не ждут вторую сторону.
            with self.lock:
                self.fresh = True
            conn.sendall(f"HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nConnection: close\r\n\r\n".encode())
            return
        time.sleep(delay)
        self.note(host, method, target, tag, 200)
        line = f"ETag: {etag}\r\n" if with_tag else ""
        reply = f"HTTP/1.1 200 OK\r\nContent-Type: text/calendar; charset=utf-8\r\nContent-Length: {len(body)}\r\n{line}Connection: close\r\n\r\n"
        conn.sendall(reply.encode() + body)


class Tunnel(socketserver.BaseRequestHandler):
    def handle(self):
        net, conn = self.server.net, self.request
        conn.settimeout(20)
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = conn.recv(4096)
            if not chunk:
                return
            data += chunk
        method, target = data.split(b"\r\n", 1)[0].decode("latin-1").split(" ")[:2]
        host = target.rsplit(":", 1)[0]
        if method != "CONNECT" or net.down or (host not in self.server.contexts and host != "silent.example.test"):
            net.note(host, method, target, None, 502)
            conn.sendall(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        conn.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")
        if host == "silent.example.test":
            net.note(host, method, target, None, 0)
            time.sleep(15)
            return
        try:
            with self.server.contexts[host].wrap_socket(conn, server_side=True) as tls:
                net.respond(tls, host)
        except (ssl.SSLError, OSError) as e:
            net.note(host, "TLS", target, None, str(e)[:80])


class Proxy(socketserver.ThreadingTCPServer):
    """CONNECT-прокси и серверы за ним: у каждого имени свой сертификат, `silent` молчит, остальных имён нет."""

    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, net):
        super().__init__(("127.0.0.1", 0), Tunnel)
        self.net = net
        self.contexts = {}
        for host, stem in ((CAL, "cal"), ("untrusted.example.test", "untrusted"), ("wrongname.example.test", "cal")):
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.load_cert_chain(path(stem + ".pem"), path(stem + ".key"))
            self.contexts[host] = ctx


# --- почтовый сервер сцен: JMAP без писем ---

CAPS = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission", "urn:ietf:params:jmap:contacts",
        "urn:ietf:params:jmap:calendars", "urn:ietf:params:jmap:principals"]
ACCOUNT = "a1"


def folder(id_, name, role, order):
    names = ["mayReadItems", "mayAddItems", "mayRemoveItems", "maySetSeen", "maySetKeywords", "mayCreateChild", "mayRename", "mayDelete", "maySubmit"]
    return {"id": id_, "name": name, "parentId": None, "role": role, "sortOrder": order, "totalEmails": 0, "unreadEmails": 0, "totalThreads": 0, "unreadThreads": 0,
            "myRights": dict.fromkeys(names, True), "isSubscribed": True}


class Mail:
    """Ровно то, что нужно живому окну: вход, папки, отправитель, контакты. На остальные вызовы — пустой ответ."""

    def __init__(self):
        self.lock = threading.Lock()
        self.sets = []
        self.cards = {"c1": {"id": "c1", "addressBookIds": {"b1": True}, "name": {"full": "Занятой Партнёр"}, "emails": {"e1": {"address": BOOKED}},
                             "calendars": {"k1": {"kind": "freeBusy", "uri": f"https://{CAL}/free-busy/booked.vfb"}}}}

    def call(self, name, args):
        empty = {"accountId": ACCOUNT, "state": "1", "list": [], "notFound": [], "ids": [], "total": 0, "position": 0, "queryState": "1", "canCalculateChanges": False,
                 "oldState": "1", "newState": "1", "hasMoreChanges": False, "created": {}, "updated": {}, "destroyed": [], "added": [], "removed": []}
        if name == "Mailbox/get":
            boxes = [folder("m1", "Inbox", "inbox", 1), folder("m2", "Sent", "sent", 2), folder("m3", "Drafts", "drafts", 3), folder("m4", "Trash", "trash", 4)]
            return dict(empty, list=boxes)
        if name == "Identity/get":
            me = {"id": "i1", "name": "Alice Stand", "email": ME, "mayDelete": False, "textSignature": "", "htmlSignature": "", "replyTo": None, "bcc": None}
            return dict(empty, list=[me])
        if name == "AddressBook/get":
            return dict(empty, list=[{"id": "b1", "name": "Contacts", "isDefault": True, "isSubscribed": True}])
        if name == "ContactCard/get":
            with self.lock:
                return dict(empty, list=list(self.cards.values()))
        if name == "ContactCard/set":
            with self.lock:
                self.sets.append(args)
                for card, patch in (args.get("update") or {}).items():
                    for key, value in patch.items():
                        if value is None:
                            self.cards[card].pop(key, None)
                        else:
                            self.cards[card][key] = value
            return dict(empty, updated=dict.fromkeys(args.get("update") or {}))
        if name.endswith("/changes"):
            return dict(empty, created=[], updated=[])
        made = {**(args.get("create") or {}), **(args.get("emails") or {})}
        if made:
            return dict(empty, created={k: {"id": f"x{k}", "blobId": "b1", "threadId": "t1", "size": 1} for k in made})
        return empty

    def handler(self):
        mail = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def reply(self, body, code=200):
                data = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                if not self.path.startswith("/.well-known/jmap"):
                    return self.reply({}, 404)
                caps = {c: {} for c in CAPS}
                caps[CAPS[0]] = {"maxSizeUpload": 50000000, "maxConcurrentUpload": 4, "maxSizeRequest": 10000000, "maxConcurrentRequests": 4, "maxCallsInRequest": 16,
                                 "maxObjectsInGet": 500, "maxObjectsInSet": 500, "collationAlgorithms": []}
                base = "http://" + self.headers["Host"]
                self.reply({"capabilities": caps, "username": ME, "state": "1", "primaryAccounts": {c: ACCOUNT for c in CAPS},
                            "accounts": {ACCOUNT: {"name": ME, "isPersonal": True, "isReadOnly": False, "accountCapabilities": {c: {} for c in CAPS}}},
                            "apiUrl": f"{base}/jmap", "downloadUrl": base + "/d/{accountId}/{blobId}/{name}?type={type}", "uploadUrl": base + "/u/{accountId}",
                            "eventSourceUrl": base + "/events?types={types}&closeafter={closeafter}&ping={ping}"})

            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                if self.path.startswith("/u/"):
                    return self.reply({"accountId": ACCOUNT, "blobId": "b1", "type": "message/rfc822", "size": len(raw)})
                body = json.loads(raw)
                out = [[name, mail.call(name, args), tag] for name, args, tag in body["methodCalls"]]
                self.reply({"methodResponses": out, "sessionState": "1"})

        return Handler


class Servers:
    def __init__(self):
        self.net, self.mail = Net(), Mail()
        self.proxy = Proxy(self.net)
        self.jmap = http.server.ThreadingHTTPServer(("127.0.0.1", 0), self.mail.handler())
        self.jmap.daemon_threads = True
        for server in (self.proxy, self.jmap):
            threading.Thread(target=server.serve_forever, daemon=True).start()

    def env(self):
        return {"POSTMAN_URL": f"http://127.0.0.1:{self.jmap.server_address[1]}", "POSTMAN_USER": ME, "POSTMAN_PASSWORD": secret("mail", "POSTMAN_NET_MAIL"),
                "HTTPS_PROXY": f"http://127.0.0.1:{self.proxy.server_address[1]}", "NO_PROXY": "", "POSTMAN_TEST_CA": path("ca.der")}


# --- сцены ---


class Play:
    def __init__(self, ctx, group, servers):
        self.ctx, self.group, self.servers = ctx, group, servers

    def run(self, scenes, on_scene=lambda _: None, drop=()):
        ctx = self.ctx
        # Разбор одной сцены: `POSTMAN_NET_ONLY=имя,имя` — только эти из списка группы.
        only = os.environ.get("POSTMAN_NET_ONLY")
        if only:
            scenes = [s for s in scenes if s in only.split(",")]
            if not scenes:
                return
        place = os.path.join(ctx.data, self.group)
        os.makedirs(place, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("STAND_", "POSTMAN_STAND_"))}
        env.update(self.servers.env())
        env.update(LOCALAPPDATA=place, POSTMAN_SCREENS_ONLY=",".join(scenes), POSTMAN_SCREENS_SHOT_ON_FAIL="1", POSTMAN_SCREENS_PREFS="1",
                   POSTMAN_NET_LDAP_READER=secret("reader", "POSTMAN_NET_LDAP_READER"))
        for name in drop:
            env.pop(name, None)
        app = subprocess.Popen([ctx.exe, "--screens", ctx.shots, "--live"], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8",
                               errors="replace")
        # Сцена без движения ждёт 200 с на шаг: при разборе ждать меньше.
        limit = threading.Timer(float(os.environ.get("POSTMAN_NET_LIMIT", "900")), app.kill)
        limit.start()
        seen, broken = [], None
        log = open(os.path.join(ctx.data, self.group + ".log"), "a", encoding="utf-8")
        try:
            for line in app.stdout:
                log.write(line)
                log.flush()
                if line.startswith("screens:"):
                    print(" ", line.strip()[:300], flush=True)
                    broken = line.strip()
                if line.startswith("net:"):
                    print(" ", line.strip()[:1500], flush=True)
                if not line.startswith("screen "):
                    continue
                scene = line.split()[1]
                seen.append(scene)
                print("сцена", scene, flush=True)
                if broken:
                    break
                on_scene(scene)
        finally:
            limit.cancel()
            log.close()
            if broken or sys.exc_info()[0]:
                app.kill()
        code = app.wait()
        if broken:
            raise AssertionError(broken)
        missing = [s for s in scenes if s not in seen]
        assert code == 0 and not missing, f"код {code}, нет сцен: {missing}"


def group_free(ctx, servers):
    """S8-42: занятость по ссылке, шаги 3–8, и отказ цепочки сертификата."""
    play = Play(ctx, "free", servers)
    net, mail = servers.net, servers.mail

    def check(scene):
        if scene == "net-s842-next":
            got = net.requests("/free-busy/guest.vfb")
            assert len(got) >= 2, f"вторая неделя не запрошена: {got}"
        if scene == "net-s842-edit":
            assert mail.sets, "правка контакта не дошла до сервера"
            assert all("calendars" not in patch for s in mail.sets for patch in s.get("update", {}).values()), "правка контакта тронула ссылку"
            assert "calendars" in mail.cards["c1"], "ссылка контакта стёрта"
        if scene == "net-s842-silent":
            statuses = net.statuses()
            assert 404 in statuses and 502 in statuses and 0 in statuses, f"не все отказы дошли до источника: {statuses}"
        if scene in ("net-tls-untrusted", "net-tls-name"):
            host = "untrusted.example.test" if scene == "net-tls-untrusted" else "wrongname.example.test"
            assert not [r for r in net.journal if r["host"] == host and r["method"] == "GET"], f"запрос прошёл к {host}"

    play.run(["net-s842-link", "net-s842-change", "net-s842-missing", "net-s842-nohost", "net-s842-silent", "net-tls-untrusted", "net-tls-name", "net-s842-vfb", "net-s842-next",
              "net-s842-contact", "net-s842-edit"], check)
    # Перезапуск: ссылка сохранилась, вводить не нужно.
    before = len(net.requests("/free-busy/guest.vfb"))
    play.run(["net-s842-kept"])
    assert len(net.requests("/free-busy/guest.vfb")) > before, "после перезапуска ссылка не спрошена"


def group_subs(ctx, servers):
    """S8-44: подписка, шаги 2–9."""
    play = Play(ctx, "subs", servers)
    net = servers.net
    target = "/public/basic.ics"
    place = os.path.join(ctx.data, "subs", "Postman", "subscriptions")

    def check(scene):
        if scene == "net-s844-304":
            got = net.requests(target)
            assert [r for r in got if r["tag"] and r["status"] == 304], f"нет ответа 304: {got}"
        if scene == "net-s844-fresh":
            got = net.requests(target)
            assert [r for r in got if r["tag"] and r["status"] == 200], f"новая версия не скачана по If-None-Match: {got}"
        if scene == "net-s844-timer":
            # 304 и новая версия по кнопке, третий условный запрос — по сроку.
            got = [r for r in net.requests(target) if r["tag"]]
            assert len(got) >= 3, f"срок вышел, а файл не спрошен: {got}"
        if scene == "net-s844-webcal":
            assert net.requests("/public/second.ics"), "webcal:// не дошёл до сервера по https"
            assert len([f for f in os.listdir(place) if f.endswith(".ics")]) == 2, os.listdir(place)

    play.run(["net-s844-add", "net-s844-webcal", "net-s844-week", "net-s844-event", "net-s844-hide", "net-s844-color", "net-s844-304", "net-s844-fresh", "net-s844-timer",
              "net-s844-busy"], check)
    # Перезапуск без сети: прежние события и «Не обновился».
    net.down = True
    play.run(["net-s844-kept"])
    net.down = False
    play.run(["net-s844-remove"])
    left = [f for f in os.listdir(place) if f.endswith((".ics", ".etag", ".tmp"))]
    assert len(left) == 0, f"файлы подписок остались: {left}"


def group_ldap(ctx, servers):
    """S5-12.5: шаги 1, 2, 4–8."""
    play = Play(ctx, "ldap", servers)
    if not slapd_ready():
        ldap_up()

    def binds():
        return docker("logs", "--tail", "20000", CONTAINER, check=False).stdout.count('BIND dn="cn=reader')

    start, seen = binds(), {}

    def check(scene):
        if scene == "net-s5125-ldaps":
            seen["ldaps"] = binds()
            assert seen["ldaps"] > start, "по ldaps:// не было входа"
        if scene == "net-s5125-plain":
            assert binds() == seen["ldaps"], "пароль ушёл по открытому каналу"
        if scene == "net-s5125-starttls":
            assert binds() > seen["ldaps"], "по StartTLS не было входа"

    silent = socket.socket()
    silent.bind(("127.0.0.1", SILENT_PORT))
    silent.listen(8)
    try:
        play.run(["net-s5125-ldaps", "net-s5125-plain", "net-s5125-starttls", "net-s5125-anon", "net-s5125-find", "net-s5125-none", "net-s5125-untrusted", "net-s5125-down",
                  "net-s5125-silent"], check)
    finally:
        silent.close()
        # Сцена сорвалась до своей уборки: записей стенда в Диспетчере учётных данных не оставляем.
        for server in (f"ldaps://localhost:{LDAPS_PORT}", f"ldap://localhost:{LDAP_PORT}"):
            subprocess.run(["cmdkey", f"/delete:Postman:ldap:{server}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


def group_ldap_chain(ctx, servers):
    """Цепочка каталога: без корня стенда `ldaps://` и StartTLS отказывают, входа нет."""
    Play(ctx, "ldap-chain", servers).run(["net-s5125-chain"], drop=("POSTMAN_TEST_CA",))


def group_no_root(ctx, servers):
    """Источник https без корня стенда: тот же сервер — недоверенный, запрос не уходит."""
    Play(ctx, "no-root", servers).run(["net-tls-noroot"], drop=("POSTMAN_TEST_CA",))
    got = [r for r in servers.net.requests("/free-busy/guest.ics") if r["method"] == "GET"]
    assert not got, f"запрос прошёл без доверия к корню: {got}"


def group_defects(ctx, servers):
    """Известные дефекты разбора в воркере (класс B). Падают, пока они не исправлены:
    `endless` — бесконечные серии: первая съедает предел развёртки на весь файл, так публикуют Outlook и Google;
    `utc` — серия с `DTSTART:…Z`: календарь разворачивает её как время пояса показа."""
    failed = []
    for scene in ("net-series-endless", "net-series-utc"):
        try:
            os.environ["POSTMAN_NET_LIMIT"] = "60"
            Play(ctx, scene, servers).run([scene])
        except AssertionError as e:
            failed.append(f"{scene}: {e}")
    assert not failed, "; ".join(failed)


class Ctx:
    def __init__(self, exe, shots, data):
        self.exe, self.shots, self.data = exe, shots, data


# `ldap` — только по имени: пишет записи в Диспетчер учётных данных Windows.
GROUPS = {"free": group_free, "subs": group_subs, "ldap-chain": group_ldap_chain, "no-root": group_no_root}
# Известные дефекты воркера: только по имени.
ON_REQUEST = {"defects": group_defects, "ldap": group_ldap}


def run(args):
    exe, shots, data = args[:3]
    ctx = Ctx(exe, shots, data)
    failed = []
    for name in args[3:] or list(GROUPS):
        print("== группа", name, flush=True)
        try:
            {**GROUPS, **ON_REQUEST}[name](ctx, Servers())
            print("== готово", name, flush=True)
        except (AssertionError, KeyError, OSError) as e:
            print("== ПАДЕНИЕ", name, str(e)[:400], flush=True)
            traceback.print_exc(limit=3)
            failed.append(name)
    print("упало групп:", len(failed), " ".join(failed), flush=True)
    sys.exit(1 if failed else 0)


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "certs":
        certs()
    elif command == "ldap":
        ldap_up()
    elif command == "run":
        run(sys.argv[2:])
    elif command == "serve":
        servers = Servers()
        print({k: v for k, v in servers.env().items() if k in ("POSTMAN_URL", "HTTPS_PROXY", "POSTMAN_TEST_CA")}, flush=True)
        threading.Event().wait()
    else:
        print(__doc__)
        sys.exit(2)


main()
