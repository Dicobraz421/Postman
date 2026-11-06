"""Вторая сторона живых сцен (`app-gpui/src/drill_live.rs`, `drill_stand.rs`), стенд — локальный контейнер Stalwart.

Запускает `postman.exe --screens … --live` и по строкам `screen <сцена>` играет коллегу: письма, события, остановка сервера.
Состояние на сервере сверяет запросами JMAP, IMAP, CalDAV и CardDAV. Пароли — только из окружения, в вывод не попадают.

  python peer.py <postman.exe> <каталог снимков> <каталог данных> [группа …]

Группы — `GROUPS` внизу; без имён — все. Окружение (см. `docs/STALWART.md`): POSTMAN_STAND_URL, POSTMAN_STAND_IMAP,
POSTMAN_STAND_USER1..3, POSTMAN_STAND_PASSWORD1..3, POSTMAN_STAND_USER, POSTMAN_STAND_PASSWORD, STAND_ADMIN, STAND_ADMIN_PASSWORD.

Настройки стенда, которые группы меняют на время прогона, возвращаются в `finally` своей группы.
"""

import base64
import concurrent.futures
import copy
import imaplib
import io
import json
import os
import re
import secrets
import ssl
import struct
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import zlib

CONTAINER = "postman-stand"
URL = os.environ["POSTMAN_STAND_URL"]
CORE = "urn:ietf:params:jmap:core"
MAIL = "urn:ietf:params:jmap:mail"
SUBMIT = "urn:ietf:params:jmap:submission"
SIEVE = "urn:ietf:params:jmap:sieve"
QUOTA = "urn:ietf:params:jmap:quota"
CALENDARS = "urn:ietf:params:jmap:calendars"
USING = [CORE, MAIL, SUBMIT]
ADMIN = "urn:stalwart:jmap"
RUNNING = set()


def http(user, password, path, body=None):
    req = urllib.request.Request(URL + path, data=None if body is None else json.dumps(body).encode())
    req.add_header("Authorization", "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode())
    req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode())


class Mailbox:
    """Ящик стенда: `n` — номер `POSTMAN_STAND_USER<n>`, пустая строка — `POSTMAN_STAND_USER` (большой ящик)."""

    def __init__(self, n="", user=None, password=None):
        self.user = user or os.environ[f"POSTMAN_STAND_USER{n}"]
        self.password = password or os.environ[f"POSTMAN_STAND_PASSWORD{n}"]
        session = http(self.user, self.password, "/jmap/session")
        self.account = session["primaryAccounts"][MAIL]
        self.session = session

    def shared(self, name):
        """Тот же вход, другой аккаунт: общий ящик."""
        other = copy.copy(self)
        other.account = next(i for i, a in self.session["accounts"].items() if name in a["name"])
        return other

    def call(self, calls, using=USING):
        body = {"using": using, "methodCalls": calls}
        return http(self.user, self.password, "/jmap", body)["methodResponses"]

    def boxes(self):
        got = self.call([["Mailbox/get", {"accountId": self.account, "ids": None, "properties": ["role", "name"]}, "0"]])
        return {m.get("role") or m["name"]: m["id"] for m in got[0][1]["list"]}

    def folder(self, name):
        """Папка по имени, нет — создать."""
        boxes = self.boxes()
        if name in boxes:
            return boxes[name]
        got = self.call([["Mailbox/set", {"accountId": self.account, "create": {"f": {"name": name, "parentId": None}}}, "0"]])
        return got[0][1]["created"]["f"]["id"]

    def send(self, to, subject, text, keep=False, rich=None):
        """Письмо через EmailSubmission; `keep` — копия в «Отправленных», иначе письмо остаётся в «Черновиках»; `rich` — свойства письма сверх текста."""
        boxes = self.boxes()
        ident = self.call([["Identity/get", {"accountId": self.account}, "0"]])[0][1]["list"][0]["id"]
        email = {
            "mailboxIds": {boxes["drafts"]: True},
            "keywords": {"$seen": True, "$draft": True},
            "from": [{"email": self.user}],
            "to": [{"email": to}],
            "subject": subject,
            "bodyValues": {"t": {"value": text}},
            "textBody": [{"partId": "t", "type": "text/plain"}],
        }
        email.update(rich or {})
        if "bodyStructure" in email:
            del email["textBody"]
        submission = {"identityId": ident, "emailId": "#m"}
        arg = {"accountId": self.account, "create": {"s": submission}}
        if keep:
            arg["onSuccessUpdateEmail"] = {"#s": {f"mailboxIds/{boxes['drafts']}": None, f"mailboxIds/{boxes['sent']}": True, "keywords/$draft": None}}
        got = self.call([
            ["Email/set", {"accountId": self.account, "create": {"m": email}}, "0"],
            ["EmailSubmission/set", arg, "1"],
        ])
        assert "s" in got[1][1].get("created", {}), got

    def send_rich(self, to, subject, text, html, name, data):
        """Письмо с HTML и вложением: так его присылает другой почтовый клиент."""
        blob = upload(self, data, "text/plain")
        self.send(to, subject, text, rich={
            "bodyValues": {"t": {"value": text}, "h": {"value": html}},
            "htmlBody": [{"partId": "h", "type": "text/html"}],
            "attachments": [{"blobId": blob, "type": "text/plain", "name": name, "disposition": "attachment"}],
        })

    def send_file(self, to, subject, name, kind, data):
        blob = upload(self, data, kind)
        self.send(to, subject, "Файл во вложении.", rich={"attachments": [{"blobId": blob, "type": kind, "name": name, "disposition": "attachment"}]})

    def text_of(self, subject):
        """Текст письма: так его видит клиент без Postman."""
        got = self.call([
            ["Email/query", {"accountId": self.account, "sort": [{"property": "receivedAt", "isAscending": False}], "limit": 200}, "0"],
            ["Email/get", {"accountId": self.account, "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}, "properties": ["subject", "textBody", "bodyValues"],
                           "fetchTextBodyValues": True}, "1"],
        ])
        letter = next(e for e in got[1][1]["list"] if subject in (e.get("subject") or ""))
        return "\n".join(v["value"] for v in letter["bodyValues"].values())

    def by_subject(self, text, box=None):
        """Письма, в теме которых есть `text`: новые письма сверху, поиск сервера здесь не нужен."""
        flt = {"inMailbox": box} if box else {}
        got = self.call([
            ["Email/query", {"accountId": self.account, "filter": flt, "sort": [{"property": "receivedAt", "isAscending": False}], "limit": 200}, "0"],
            ["Email/get", {"accountId": self.account, "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"},
                           "properties": ["subject", "keywords", "mailboxIds", "from", "to"]}, "1"],
        ])
        return [e for e in got[1][1]["list"] if text in (e.get("subject") or "")]

    def top(self):
        got = self.call([
            ["Email/query", {"accountId": self.account, "sort": [{"property": "receivedAt", "isAscending": False}], "limit": 1}, "0"],
            ["Email/get", {"accountId": self.account, "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}, "properties": ["subject", "keywords"]}, "1"],
        ])
        return got[1][1]["list"][0]

    def wait(self, text, box=None, seconds=30):
        """Дождаться письма с такой темой."""
        for _ in range(seconds * 2):
            found = self.by_subject(text, box)
            if found:
                return found[0]
            time.sleep(0.5)
        raise AssertionError(f"письмо «{text}» не пришло к {self.user}")

    def scripts(self):
        got = self.call([["SieveScript/get", {"accountId": self.account, "ids": None}, "0"]], [CORE, SIEVE])
        return got[0][1]["list"]

    def clear_scripts(self):
        """Чистый ящик правил: чужие и прошлые скрипты сцене мешают."""
        ids = [s["id"] for s in self.scripts()]
        if ids:
            self.call([["SieveScript/set", {"accountId": self.account, "onSuccessDeactivateScript": True}, "0"]], [CORE, SIEVE])
            got = self.call([["SieveScript/set", {"accountId": self.account, "destroy": ids}, "0"]], [CORE, SIEVE])
            assert not got[0][1].get("notDestroyed"), got

    def add_script(self, name, text):
        blob = upload(self, text)
        got = self.call([["SieveScript/set", {"accountId": self.account, "create": {"s": {"name": name, "blobId": blob}},
                                              "onSuccessActivateScript": "#s"}, "0"]], [CORE, SIEVE])
        assert "s" in got[0][1].get("created", {}), got

    def calendar_titles(self):
        """Названия событий всех календарей (JMAP, `CalendarEvent/query`)."""
        got = self.call([
            ["CalendarEvent/query", {"accountId": self.account, "limit": 200}, "0"],
            ["CalendarEvent/get", {"accountId": self.account, "#ids": {"resultOf": "0", "name": "CalendarEvent/query", "path": "/ids"},
                                   "properties": ["title", "@type"]}, "1"],
        ], [CORE, CALENDARS])
        return [e.get("title") for e in got[1][1]["list"]]


def upload(box, text, kind="application/sieve"):
    path = box.session["uploadUrl"].replace("{accountId}", box.account)
    path = path[path.index("/jmap"):]
    req = urllib.request.Request(URL + path, data=text if isinstance(text, bytes) else text.encode(), method="POST")
    req.add_header("Authorization", "Basic " + base64.b64encode(f"{box.user}:{box.password}".encode()).decode())
    req.add_header("Content-Type", kind)
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode())["blobId"]


class Dav:
    """Календарь по умолчанию по CalDAV: другой клиент и свидетель состояния на сервере (вместо Thunderbird)."""

    def __init__(self, box):
        self.box = box
        self.path = f"/dav/cal/{urllib.parse.quote(box.user, safe='')}/default/"

    def request(self, method, path, body=None, kind="application/xml; charset=utf-8", depth="1"):
        req = urllib.request.Request(URL + path, data=None if body is None else body.encode(), method=method)
        token = base64.b64encode(f"{self.box.user}:{self.box.password}".encode()).decode()
        req.add_header("Authorization", "Basic " + token)
        req.add_header("Content-Type", kind)
        req.add_header("Depth", depth)
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                return r.read().decode()
        except urllib.error.HTTPError as e:
            raise AssertionError(f"{method} {path}: {e.code} {e.read().decode()[:300]}") from e

    def items(self, comp):
        """Записи календаря: {адрес: текст iCalendar как на сервере}."""
        query = ('<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop>'
                 f'<c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="{comp}"/></c:comp-filter></c:filter></c:calendar-query>')
        text = self.request("REPORT", self.path, query)
        out = {}
        for part in text.split("response>"):
            href = re.search(r"href>([^<]+)<", part)
            data = re.search(r"<!\[CDATA\[(.*?)\]\]>", part, re.S)
            if href and data:
                out[href.group(1)] = data.group(1)
        return out

    def find(self, comp, text):
        return next(((h, b) for h, b in self.items(comp).items() if text in b), (None, None))

    def wait(self, comp, text, seconds=15):
        for _ in range(seconds * 2):
            href, body = self.find(comp, text)
            if href:
                return href, body
            time.sleep(0.5)
        raise AssertionError(f"на сервере нет {comp} «{text}»")

    def gone(self, comp, text, seconds=15):
        for _ in range(seconds * 2):
            if not self.find(comp, text)[0]:
                return
            time.sleep(0.5)
        raise AssertionError(f"на сервере остался {comp} «{text}»")

    def put(self, href, ics):
        self.request("PUT", href, ics, "text/calendar; charset=utf-8", "0")

    def new_event(self, title, rule=None, days=0):
        uid = "peer-" + secrets.token_hex(6)
        now = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        at = time.time() + days * 86400
        start = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime(at + 3600))
        end = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime(at + 5400))
        ics = ("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//postman stand peer//RU\r\nBEGIN:VEVENT\r\n"
               f"UID:{uid}\r\nDTSTAMP:{now}\r\nDTSTART:{start}\r\nDTEND:{end}\r\nSUMMARY:{title}\r\n" + (f"RRULE:{rule}\r\n" if rule else "") + "END:VEVENT\r\nEND:VCALENDAR\r\n")
        self.put(self.path + uid + ".ics", ics)
        return self.path + uid + ".ics"

    def delete(self, href):
        self.request("DELETE", href, None, depth="0")

    def principal(self):
        body = '<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>'
        text = self.request("PROPFIND", self.path, body, depth="0")
        return re.search(r"current-user-principal>\s*<[^>]*href>([^<]+)<", text).group(1)

    def share(self, principal):
        """Открыть календарь коллеге: на Stalwart занятость видна только с обоими правами (`docs/STALWART.md`, п. 6)."""
        self.request("ACL", self.path, ('<d:acl xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:ace><d:principal><d:href>' + principal +
                                        '</d:href></d:principal><d:grant><d:privilege><d:read/></d:privilege><d:privilege><c:read-free-busy/></d:privilege>'
                                        '</d:grant></d:ace></d:acl>'), depth="0")

    def unshare(self):
        self.request("ACL", self.path, '<d:acl xmlns:d="DAV:"/>', depth="0")

    def new_todo(self, title):
        uid = "peer-" + secrets.token_hex(6)
        now = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        ics = ("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//postman stand peer//RU\r\nBEGIN:VTODO\r\n"
               f"UID:{uid}\r\nDTSTAMP:{now}\r\nSUMMARY:{title}\r\nEND:VTODO\r\nEND:VCALENDAR\r\n")
        self.put(self.path + uid + ".ics", ics)


class Admin:
    """Настройка стенда объектами `x:*`: квота, временная учётная запись, смена пароля."""

    def __init__(self):
        self.user, self.password = os.environ["STAND_ADMIN"], os.environ["STAND_ADMIN_PASSWORD"]
        self.account = http(self.user, self.password, "/jmap/session")["primaryAccounts"][ADMIN]

    def call(self, calls):
        return http(self.user, self.password, "/jmap", {"using": [CORE, ADMIN], "methodCalls": calls})["methodResponses"]

    def accounts(self):
        got = self.call([["x:Account/get", {"accountId": self.account, "ids": None, "properties": ["id", "emailAddress", "usedDiskQuota"]}, "0"]])
        return {a.get("emailAddress"): a for a in got[0][1]["list"]}

    def quota(self, email, limit):
        """Предел ящика в байтах; `None` — снять."""
        update = {self.accounts()[email]["id"]: {"quotas": {"maxDiskQuota": limit} if limit else {}}}
        got = self.call([["x:Account/set", {"accountId": self.account, "update": update}, "0"]])
        assert not got[0][1].get("notUpdated"), got

    def temp_user(self, name, password):
        domain = self.call([["x:Domain/get", {"accountId": self.account, "ids": None}, "0"]])[0][1]["list"][0]["id"]
        new = {"@type": "User", "name": name, "domainId": domain, "description": f"Test {name}", "roles": {"@type": "User"},
               "credentials": {"0": {"@type": "Password", "secret": password}}}
        got = self.call([["x:Account/set", {"accountId": self.account, "create": {"t": new}}, "0"]])
        assert "t" in got[0][1].get("created", {}), "учётная запись не создана"
        return got[0][1]["created"]["t"]["id"]

    def new_password(self, account, password):
        got = self.call([["x:Account/set", {"accountId": self.account, "update": {account: {"credentials/0/secret": password}}}, "0"]])
        assert not got[0][1].get("notUpdated"), "пароль не сменён"

    def destroy(self, account):
        self.call([["x:Account/set", {"accountId": self.account, "destroy": [account]}, "0"]])


class Play:
    """Запуски Postman одной группы: каталог данных один — второй запуск видит настройки и кэш первого."""

    def __init__(self, ctx, group):
        self.ctx, self.group = ctx, group

    def run(self, scenes, on_scene, user, password, url=URL, data=None, lang="ru", extra=None, hidden=()):
        """`hidden` — сцены без снимка: окно закрывается раньше него."""
        ctx = self.ctx
        place = os.path.join(ctx.data, data or self.group)
        os.makedirs(place, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("STAND_", "POSTMAN_STAND_"))}
        env.update(LOCALAPPDATA=place, POSTMAN_SCREENS_ONLY=",".join([*scenes, *hidden]), POSTMAN_URL=url, POSTMAN_USER=user, POSTMAN_PASSWORD=password,
                   POSTMAN_STAND_STAMP=ctx.stamp, POSTMAN_SCREENS_SHOT_ON_FAIL="1")
        env.update(extra or {})
        app = subprocess.Popen([ctx.exe, "--screens", ctx.shots, "--live", "--lang", lang], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               text=True, encoding="utf-8", errors="replace")
        RUNNING.add(app)
        seen, broken = [], None
        try:
            for line in app.stdout:
                if line.startswith("screens:"):
                    print(" ", line.strip()[:300], flush=True)
                    broken = line.strip()
                if "panicked" in line:
                    print(" ", line.strip()[:300], flush=True)
                if os.environ.get("POSTMAN_LOG") and "WARN" in line:
                    print(" ", line.strip()[:300], flush=True)
                if not line.startswith("screen "):
                    continue
                scene = line.split()[1]
                seen.append(scene)
                print("сцена", scene, flush=True)
                if broken:
                    break
                on_scene(scene)
        finally:
            # Сцена сорвалась или сверка упала: окно ждать незачем.
            if broken or sys.exc_info()[0]:
                app.kill()
        code = app.wait()
        RUNNING.discard(app)
        if broken:
            raise AssertionError(broken)
        missing = [s for s in scenes if s not in seen]
        assert code == 0 and not missing, f"код {code}, нет сцен: {missing}"


# --- группы ---


def group_live(ctx):
    """Разделы 9, 34, T1-22: письмо, push, поиск, обрыв связи, «Отправленные»."""
    play = Play(ctx, "live")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    two.send(one.user, f"Договор {stamp}", "Письмо по договору о сроках поставки оборудования. It mentions a quarterly budget.")
    one.wait(f"Договор {stamp}")
    # T1-22: письмо в «Отправленных», иначе сцене `sent` не на чем стоять.
    one.send(two.user, f"Отправленное {stamp}", "Копия лежит в «Отправленных».", keep=True)
    assert one.by_subject(f"Отправленное {stamp}", one.boxes()["sent"]), "нет копии в «Отправленных»"

    def on_scene(scene):
        if scene == "live-read":
            top = one.top()
            assert "$seen" in top["keywords"], f"на сервере не прочитано: {top}"
            print("сервер: письмо прочитано")
            two.send(one.user, f"Postman push {stamp}", "Новое письмо без перезапуска.")
        if scene == "live-search-2":
            subprocess.run(["docker", "stop", "-t", "1", CONTAINER], check=True, capture_output=True)
            print("сервер остановлен")
        if scene == "live-offline":
            subprocess.run(["docker", "start", CONTAINER], check=True, capture_output=True)
            print("сервер запущен")

    only = ["list", "letter", "sent", "sent-letter", "live-sync", "live-read", "live-push", "live-search-1", "live-search-2", "live-offline", "live-online"]
    try:
        play.run(only, on_scene, one.user, one.password)
    finally:
        subprocess.run(["docker", "start", CONTAINER], capture_output=True)
        wait_stand()


def group_rules(ctx):
    """S5-26 шаги 2, 3; раздел 16 шаги 2–5, 7. Правила — у user2, письма шлёт user1."""
    play = Play(ctx, "rules")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    two.clear_scripts()
    projects = two.folder("Проекты")
    inbox = two.boxes()["inbox"]

    def on_scene(scene):
        if scene == "st-s526-rule":
            active = [s for s in two.scripts() if s.get("isActive")]
            assert active, "на сервере нет активного скрипта"
            print("сервер: скрипт правил активен:", active[0]["name"])
            one.send(two.user, f"тендер проверка {stamp}", "Правило должно поставить категорию.")
        if scene == "st-s526-label":
            got = two.wait(f"тендер проверка {stamp}")
            words = [k for k in got["keywords"] if k.startswith("pm.cat.")]
            assert words, f"нет категории на сервере: {got['keywords']}"
            print("сервер: категория", words)
        if scene == "st-s526-gone":
            one.send(two.user, f"тендер снова {stamp}", "Правила нет: без категории.")
        if scene == "st-s526-plain":
            got = two.wait(f"тендер снова {stamp}")
            assert not [k for k in got["keywords"] if k.startswith("pm.cat.")], got["keywords"]
            print("сервер: без категории")
        if scene == "st-16-move":
            one.send(two.user, f"Проект Альфа {stamp}", "Правило переносит в папку.")
        if scene == "st-16-moved":
            got = two.wait(f"Проект Альфа {stamp}")
            assert projects in got["mailboxIds"] and inbox not in got["mailboxIds"], got
            print("сервер: письмо в «Проектах»")
        if scene == "st-16-off":
            one.send(two.user, f"Проект Бета {stamp}", "Правило выключено.")
        if scene == "st-16-inbox":
            got = two.wait(f"Проект Бета {stamp}")
            assert inbox in got["mailboxIds"] and projects not in got["mailboxIds"], got
            print("сервер: письмо во «Входящих»")
        if scene == "st-16-foreign":
            active = [s["name"] for s in two.scripts() if s.get("isActive")]
            assert active == ["from-webmail"], f"отмена изменила скрипт: {active}"
            print("сервер: чужой скрипт на месте")

    only = ["st-s526-rule", "st-s526-label", "st-s526-gone", "st-s526-plain", "st-16-move", "st-16-moved", "st-16-off", "st-16-inbox", "st-16-address",
            "st-16-delete"]
    try:
        play.run(only, on_scene, two.user, two.password)
        # Чужой скрипт виден со следующего входа: панель читает правила один раз.
        two.add_script("from-webmail", 'require "fileinto";\nif header :contains "subject" "чужой" { fileinto "Junk Mail"; }\n')
        print("чужой скрипт активен")
        play.run(["st-16-foreign"], on_scene, two.user, two.password)
    finally:
        two.clear_scripts()


def group_vacation(ctx):
    """S5-29 шаги 2–8, раздел 15 шаги 2–7: автоответ user2, письма от user1; квота — настройкой стенда на время прогона."""
    play = Play(ctx, "vacation")
    two = Mailbox(2)
    stamp = ctx.stamp
    two.clear_scripts()
    admin = Admin()
    # Свежий отправитель: сервер помнит, кому уже отвечал, 4 дня, и ответ прошлому прогону или тесту стенда глушит этот.
    name, secret = "drill" + secrets.token_hex(2), secrets.token_urlsafe(18)
    temp = admin.temp_user(name, secret)
    one = Mailbox(user=f"{name}@{two.user.split('@')[1]}", password=secret)

    def saved(scene):
        if scene == "st-s529-save":
            # Stalwart не даёт записать свой скрипт `vacation`: один текст всем, события «Вне офиса» нет (S5-29, шаг 4, — находка).
            assert "Вне офиса" not in two.calendar_titles(), "сервер принял скрипт автоответа: вернуть проверку события"
            one.send(two.user, f"Вопрос {stamp}", "Первое письмо в период автоответа.")
            # Один текст всем (`AutoReply::primary`): коллеге приходит тема закладки «Внешним».
            try:
                reply = one.wait("Вне компании", seconds=20)
                print("сервер: ответ, тема:", reply["subject"])
            except AssertionError:
                print("находка: автоответа нет; письма:", [e.get("subject") for e in one.by_subject("")],
                      "скрипты:", [(x["name"], x.get("isActive")) for x in two.scripts()], flush=True)
                return
            before = len(one.by_subject("Вне компании"))
            one.send(two.user, f"Вопрос второй {stamp}", "Второе письмо: без ответа 4 дня.")
            two.wait(f"Вопрос второй {stamp}")
            time.sleep(6)
            assert len(one.by_subject("Вне компании")) == before, "второй автоответ"
            print("сервер: на второе письмо ответа нет")

    def off(scene):
        if scene == "st-s529-off":
            before = len(one.by_subject("Вне компании"))
            one.send(two.user, f"После выключения {stamp}", "Автоответа быть не должно.")
            two.wait(f"После выключения {stamp}")
            time.sleep(5)
            assert len(one.by_subject("Вне компании")) == before, "автоответ после выключения"
            print("сервер: автоответа нет")

    try:
        play.run(["st-s529-format", "st-s529-cancel", "st-s529-external", "st-15-order", "st-s529-save"], saved, two.user, two.password)
        play.run(["st-s529-kept", "st-s529-undo", "st-s529-off"], off, two.user, two.password)
        used = admin.accounts()[two.user]["usedDiskQuota"]
        admin.quota(two.user, used + used // 20 + 1)
        play.run(["st-s529-quota", "st-s529-clean"], lambda _: None, two.user, two.password)
    finally:
        admin.quota(two.user, None)
        admin.destroy(temp)
        two.clear_scripts()


def group_tasks(ctx):
    """S6-05 шаг 7, S6-06 шаг 4, S6-08 шаг 3: задачи user1, другой клиент — CalDAV, второй Postman — второе окно той же учётной записи."""
    play = Play(ctx, "tasks")
    one = Mailbox(1)
    dav = Dav(one)
    first, edited, server = f"Задача стенда {ctx.stamp}", f"Задача правка {ctx.stamp}", f"С сервера {ctx.stamp}"

    def on_scene(scene):
        if scene == "st-s605-new":
            href, body = dav.wait("VTODO", first)
            print("сервер: VTODO из Postman")
            dav.put(href, body.replace(first, edited))
            dav.new_todo(server)
            print("другой клиент: правка и новая задача")
        if scene == "st-s606-card":
            for _ in range(20):
                _, body = dav.wait("VTODO", edited)
                if "PRIORITY:" in body and "PRIORITY:0" not in body:
                    break
                time.sleep(0.5)
            print("сервер: PRIORITY", re.findall(r"PRIORITY:\d+", body))
            assert re.search(r"PRIORITY:[1-4]\b", body), body
        if scene == "st-s608-block":
            for _ in range(20):
                if edited in one.calendar_titles():
                    break
                time.sleep(0.5)
            print("сервер: событие-блок")
            dav.wait("VEVENT", edited)
        if scene == "st-s608-gone":
            dav.gone("VEVENT", edited)
            print("сервер: блока нет")
        if scene == "st-s605-done":
            for _ in range(30):
                _, body = dav.wait("VTODO", edited)
                if "STATUS:COMPLETED" in body:
                    break
                time.sleep(0.5)
            assert "STATUS:COMPLETED" in body, body
            print("сервер: STATUS:COMPLETED")
        if scene == "st-s605-delete":
            dav.gone("VTODO", server)
            print("сервер: задачи нет")

    main = ["st-s605-new", "st-s605-push", "st-s606-card", "st-s608-block", "st-s608-week", "st-s608-gone", "st-s605-done", "st-s605-delete"]
    with concurrent.futures.ThreadPoolExecutor(1) as pool:
        jobs = []

        def second(scene):
            # S6-06, шаг 4: второй Postman открыт раньше правки: она приходит ему сама.
            if scene == "st-s606-open":
                jobs.append(pool.submit(play.run, main, on_scene, one.user, one.password))

        play.run(["st-s606-open", "st-s606-seen"], second, one.user, one.password, data="tasks-second")
        for job in jobs:
            job.result()


def group_card(ctx):
    """S6-05 шаги 3–6, S6-06 шаги 1–3, S6-07 шаги 1, 2, S6-08 шаг 1: задачи user1 на живом ящике; свидетель — CalDAV."""
    play = Play(ctx, "card")
    one, two = Mailbox(1), Mailbox(2)
    dav = Dav(one)
    stamp = ctx.stamp
    card, renamed, project, letter = f"Карточка стенда {stamp}", f"Карточка новая {stamp}", f"Проект стенда {stamp}", f"Письмо со сроком {stamp}"
    two.send(one.user, letter, "Письмо станет задачей.")
    one.wait(letter, one.boxes()["inbox"])

    def waiting(title, good, what):
        for _ in range(40):
            href, body = dav.find("VTODO", title)
            if href and good(body):
                return body
            time.sleep(0.5)
        raise AssertionError(f"{what}: {body!r}")

    def on_scene(scene):
        if scene == "st-s605-today":
            waiting(card, lambda b: "DUE" in b, "нет срока сегодня на сервере")
            print("сервер: задача со сроком", flush=True)
        if scene == "st-s605-closed":
            waiting(card, lambda b: "STATUS:COMPLETED" in b, "не выполнена на сервере")
            print("сервер: выполнена", flush=True)
        if scene == "st-s605-undone":
            waiting(card, lambda b: "STATUS:COMPLETED" not in b, "выполнение не снято на сервере")
            print("сервер: снова в работе", flush=True)
        if scene == "st-s605-keys":
            waiting(renamed, lambda b: "DUE" in b, "нет нового названия на сервере")
            print("сервер: срок и новое название", flush=True)
        if scene == "st-s605-del":
            dav.gone("VTODO", renamed)
            print("сервер: удалена", flush=True)
        if scene == "st-s605-undel":
            dav.wait("VTODO", renamed)
            print("сервер: вернулась", flush=True)
        if scene == "st-s606-fields":
            body = waiting(renamed, lambda b: "VALARM" in b and project in b.replace("\r\n ", "") and "pm.cat." in b, "поля не на сервере")
            print("сервер: напоминание, проект, категория", flush=True)
        if scene == "st-s606-subs":
            for _ in range(40):
                subs = [b for b in dav.items("VTODO").values() if "RELATED-TO" in b and ("Первая" in b or "Вторая" in b)]
                if len(subs) >= 2 and any("STATUS:COMPLETED" in b for b in subs):
                    break
                time.sleep(0.5)
            assert len(subs) >= 2 and any("STATUS:COMPLETED" in b for b in subs), subs
            print("сервер: подзадачи, отмеченная выполнена", flush=True)
        if scene == "st-s606-notes":
            waiting(renamed, lambda b: "жирным" in b.replace("\r\n ", ""), "заметки не на сервере")
            print("сервер: заметки", flush=True)
        if scene == "st-s607-repeat":
            waiting(renamed, lambda b: "FREQ=MONTHLY" in b, "нет RRULE")
            print("сервер: повтор", flush=True)
        if scene == "st-s607-next":
            wait_for(lambda: len([b for b in dav.items("VTODO").values() if renamed in b.replace("\r\n ", "")]) >= 2, "нет копии на сервере")
            print("сервер: копия со следующим сроком", flush=True)

    only = ["st-s605-today", "st-s605-space", "st-s605-closed", "st-s605-undone", "st-s605-keys", "st-s605-del", "st-s605-undel", "st-s606-fields",
            "st-s606-subs", "st-s606-notes", "st-s607-repeat", "st-s607-next", "st-s607-undo", "st-s608-card", "st-s605-mail", "st-s605-mail-done",
            "st-s605-mail-undo"]
    try:
        play.run(only, on_scene, one.user, one.password)
    finally:
        for href, body in dav.items("VTODO").items():
            if stamp in body or "RELATED-TO" in body:
                dav.delete(href)


def group_full(ctx):
    """S7-03а шаги 1–6, S5-21 шаг 8, S5-22 шаг 4, T1-06: живой ящик полного сервера."""
    play = Play(ctx, "full")
    one, two, three = Mailbox(1), Mailbox(2), Mailbox(3)
    dav, peer = Dav(one), Dav(two)
    stamp = ctx.stamp
    # Встреча на другой неделе: поиск идёт на сервер.
    # Среда: рабочая неделя показывает её при любом дне запуска.
    far = dav.new_event(f"Встреча далеко {stamp}", days=28 + (2 - time.localtime().tm_wday) % 7)
    near = peer.new_event(f"Событие коллеги {stamp}")
    try:
        peer.share(dav.principal())
        print("другой клиент: user2 открыл календарь user1", flush=True)
        play.run(["st-s703a-rail", "st-s703a-settings", "st-s703a-sections", "st-s703a-palette", "st-s703a-contacts", "st-s703a-keys", "st-s521-found", "st-s521-open", "st-s522-week"],
                 lambda _: None, one.user, one.password)
    finally:
        peer.unshare()
        peer.delete(near)
        dav.delete(far)
    # Группы папок — у сотрудника общего ящика.
    play.run(["st-t106-fold", "st-t106-kept", "st-t106-branch"], lambda _: None, three.user, three.password, data="full-three")


def group_own(ctx):
    """S7-07б шаги 1–3: своя встреча по JMAP (user1 → user3) и по CalDAV (user1 → user2): свидетель — календарь участника."""
    one, two, three = Mailbox(1), Mailbox(2), Mailbox(3)
    host = os.environ["POSTMAN_STAND_IMAP"]
    standards = f"imaps://{host}?smtp={os.environ['POSTMAN_STAND_SMTP']}&dav=https://{host.split(':')[0]}"

    def round_trip(name, url, guest):
        play = Play(ctx, name)
        stamp = ctx.stamp
        extra = {"POSTMAN_STAND_PEER": guest.user}
        first, second = f"Своя встреча {stamp}", f"Своя встреча перенесена {stamp}"
        witness = Dav(guest)
        boss = lambda scenes, seen=lambda _: None: play.run(scenes, seen, one.user, one.password, url=url, extra=extra)
        boss(["st-s707-new", "st-s707-buttons"])
        witness.wait("VEVENT", first, seconds=30)
        print(name, "сервер: встреча в календаре участника", flush=True)
        play.run(["st-s707-foreign"], lambda _: None, guest.user, guest.password, url=url, data=name + "-guest")

        def seen(scene):
            if scene == "st-s707-edit":
                witness.wait("VEVENT", second, seconds=30)
                print(name, "сервер: новая тема у участника", flush=True)
            if scene == "st-s707-cancel":
                for _ in range(60):
                    _, body = witness.find("VEVENT", second)
                    if not body or "STATUS:CANCELLED" in body:
                        break
                    time.sleep(0.5)
                assert not body or "STATUS:CANCELLED" in body, body
                print(name, "сервер: у участника встречи нет или она отменена", flush=True)

        boss(["st-s707-edit", "st-s707-cancel"], seen)

    round_trip("own-jmap", URL, three)
    round_trip("own-dav", standards, two)


def png(width, height):
    """Картинка одного цвета."""
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = (b"\0" + b"\x30\x70\xc0" * width) * height
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def ooxml(parts):
    """Файл Office: части ZIP-архива."""
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for name, text in parts.items():
            z.writestr(name, text)
    return out.getvalue()


def book():
    """Книга Excel на два листа, строки вписаны прямо в лист."""
    types = ('<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
             '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'
             '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
             '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
             '<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>')
    rels = ('<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>')
    workbook = ('<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
                'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Январь" sheetId="1" r:id="rId1"/>'
                '<sheet name="Февраль" sheetId="2" r:id="rId2"/></sheets></workbook>')
    links = ('<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
             '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
             '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>')
    sheet = lambda v: ('<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>'
                       f'<row r="1"><c r="A1" t="inlineStr"><is><t>Статья</t></is></c><c r="B1" t="inlineStr"><is><t>Сумма</t></is></c></row>'
                       f'<row r="2"><c r="A2" t="inlineStr"><is><t>Связь</t></is></c><c r="B2"><v>{v}</v></c></row></sheetData></worksheet>')
    return ooxml({"[Content_Types].xml": types, "_rels/.rels": rels, "xl/workbook.xml": workbook, "xl/_rels/workbook.xml.rels": links,
                  "xl/worksheets/sheet1.xml": sheet(120), "xl/worksheets/sheet2.xml": sheet(340)})


def slides():
    """Презентация на два слайда."""
    types = ('<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
             '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'
             '<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>'
             '<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>'
             '<Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/></Types>')
    rels = ('<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>')
    main = ('<?xml version="1.0" encoding="UTF-8"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" '
            'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>')
    links = ('<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
             '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>'
             '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>')
    slide = lambda t: ('<?xml version="1.0" encoding="UTF-8"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" '
                       'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Заголовок"/><p:cNvSpPr/>'
                       f'<p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>')
    return ooxml({"[Content_Types].xml": types, "_rels/.rels": rels, "ppt/presentation.xml": main, "ppt/_rels/presentation.xml.rels": links,
                  "ppt/slides/slide1.xml": slide("Первый слайд"), "ppt/slides/slide2.xml": slide("Второй слайд")})


def document():
    """Документ Word."""
    types = ('<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
             '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'
             '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
    rels = ('<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
    body = ('<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>'
            '<w:p><w:r><w:t>Текст документа.</w:t></w:r></w:p></w:body></w:document>')
    return ooxml({"[Content_Types].xml": types, "_rels/.rels": rels, "word/document.xml": body})


def group_files(ctx):
    """S8-23 шаги 1–5, T1-13в шаг 1: письма с файлами: файлы собирает `peer.py`, в репозитории образцов нет."""
    play = Play(ctx, "files")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    eml = (f"From: Коллега <peer@example.test>\r\nTo: {one.user}\r\nSubject: Вложенное письмо\r\nDate: Fri, 06 Nov 2026 10:00:00 +0300\r\n"
           "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nТекст вложенного письма.\r\n").encode()
    OFFICE = "application/vnd.openxmlformats-officedocument."
    files = [("Файл книга", "книга.xlsx", OFFICE + "spreadsheetml.sheet", book()),
             ("Файл слайды", "слайды.pptx", OFFICE + "presentationml.presentation", slides()),
             ("Файл таблица", "таблица.csv", "text/csv", "Статья;Сумма\r\nСвязь;120\r\n".encode()),
             ("Файл письмо", "письмо.eml", "message/rfc822", eml),
             ("Файл разметка", "заметка.md", "text/markdown", "# Заголовок\n\nТекст заметки.\n".encode()),
             ("Файл документ", "документ.docx", OFFICE + "wordprocessingml.document", document())]
    for subject, name, kind, data in files:
        two.send_file(one.user, f"{subject} {stamp}", name, kind, data)
    # T1-10, T1-10в: широкая, узкая и с размером из HTML.
    pictures = {"wide": png(1800, 300), "narrow": png(120, 40), "sized": png(800, 400)}
    parts = [{"blobId": upload(two, data, "image/png"), "type": "image/png", "cid": f"{name}@example.test", "disposition": "inline", "name": f"{name}.png"} for name, data in pictures.items()]
    html = ('<p>Широкая:</p><img src="cid:wide@example.test"><p>Узкая:</p><img src="cid:narrow@example.test">'
            '<p>С размером из HTML:</p><img src="cid:sized@example.test" width="200" height="100">')
    two.send(one.user, f"Письмо с картинками {stamp}", "", rich={"bodyValues": {"h": {"value": html}},
                                                               "bodyStructure": {"type": "multipart/related", "subParts": [{"partId": "h", "type": "text/html"}, *parts]}})
    for subject, *_ in files:
        one.wait(f"{subject} {stamp}", one.boxes()["inbox"], seconds=60)
    play.run(["st-s823-book", "st-s823-slides", "st-s823-csv", "st-s823-eml", "st-s823-md", "st-t113-docx", "st-t110-images"], lambda _: None, one.user, one.password)


def group_forward(ctx):
    """T1-17 шаги 1–3: письма коллеги без тел в кэше; письмо с тремя вложениями уходит и читается на сервере."""
    play = Play(ctx, "forward")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    for word in ("раз", "два", "три"):
        two.send(one.user, f"Пересылка {word} {stamp}", "Письмо для пересылки.")
        one.wait(f"Пересылка {word} {stamp}", one.boxes()["inbox"])

    def on_scene(scene):
        if scene == "st-t117-many":
            got = one.wait(f"Три письма вложениями {stamp}", one.boxes()["inbox"], seconds=30)
            parts = one.call([["Email/get", {"accountId": one.account, "ids": [got["id"]], "properties": ["attachments"]}, "0"]])[0][1]["list"][0]["attachments"]
            assert len(parts) == 3 and all(p["type"] == "message/rfc822" for p in parts), parts
            print("сервер: три вложения message/rfc822", flush=True)

    play.run(["st-t117-many", "st-t117-one"], on_scene, one.user, one.password)


def group_more(ctx):
    """S5-12 Ф4 шаг 9, T1-02б шаг 5, T1-23 шаги 1–3 на живом ящике user1, S6-11 шаги 1–3, 6 окнами user1 и user2."""
    play = Play(ctx, "more")
    one, two = Mailbox(1), Mailbox(2)
    extra = {"POSTMAN_STAND_PEER": two.user}
    play.run(["st-s512-own", "st-t102-plain", "st-t123-menu", "st-t123-mine"], lambda _: None, one.user, one.password, extra=extra)
    # Поручение — как в S6-09: окно user1, затем окно user2 находит его у себя.
    boss = Play(ctx, "more-boss")
    boss.run(["st-s609-assign", "st-s611-by-me"], lambda _: None, one.user, one.password,
             extra={**extra, "POSTMAN_STAND_WATCHER": Mailbox(3).user})
    two.wait(f"Поручение: Поручение стенда {ctx.stamp}", two.boxes()["inbox"], seconds=30)
    play.run(["st-s611-to-me", "st-s611-foreign"], lambda _: None, two.user, two.password, data="more-second")
    boss.run(["st-s611-delete"], lambda _: None, one.user, one.password, extra=extra)


def group_deleg(ctx):
    """S6-09 шаг 8, S6-11 шаги 4, 5, 8: два окна Postman (user1, user2), наблюдатель user3 — письма на сервере."""
    play = Play(ctx, "deleg")
    one, two, three = Mailbox(1), Mailbox(2), Mailbox(3)
    order, second = f"Поручение стенда {ctx.stamp}", f"Поручение второе {ctx.stamp}"
    names = {"POSTMAN_STAND_PEER": two.user, "POSTMAN_STAND_WATCHER": three.user}

    def boss(scene):
        if scene == "st-s609-assign":
            two.wait(f"Поручение: {order}")
            text = two.text_of(f"Поручение: {order}")
            assert order in text, text
            print("сервер: письмо-поручение у user2 читается без Postman")
        if scene == "st-s609-accept":
            three.wait(f"Результат принят: {order}", seconds=30)
            print("сервер: письмо наблюдателю «Результат принят»")
        if scene == "st-s611-remind":
            two.wait(f"Напоминание: {second}")
            print("сервер: письмо-напоминание у исполнителя")
        if scene == "st-s611-undo":
            time.sleep(14)
            assert not two.by_subject(f"Поручение отозвано: {second}"), "письмо отмены ушло"
            print("сервер: письма отмены нет")
        if scene == "st-s611-revoke":
            two.wait(f"Поручение отозвано: {second}", seconds=40)
            print("сервер: письмо отмены пришло")

    run_boss = lambda: play.run(["st-s609-assign", "st-s609-accept", "st-s611-remind", "st-s611-undo", "st-s611-revoke", "st-s611-wait"], boss, one.user, one.password, extra=names)
    run_worker = lambda: play.run(["st-s609-got", "st-s609-work", "st-s609-closed"], lambda _: None, two.user, two.password, data="deleg-second")
    together(run_boss, run_worker)


def group_shared(ctx):
    """S5-28 шаг 8, раздел 18 шаги 1–5: два сотрудника `shared-sales` (user3 и user1), письма — от user2."""
    play = Play(ctx, "shared")
    one, two, three = Mailbox(1), Mailbox(2), Mailbox(3)
    stamp = ctx.stamp
    domain = one.user.split("@")[1]
    sales_mail, ro_mail = f"shared-sales@{domain}", f"shared-ro@{domain}"
    sales = three.shared("shared-sales")
    subject = f"Общему ящику {stamp}"

    def first(scene):
        if scene == "st-18-tree":
            two.send(sales_mail, subject, "Письмо в общий ящик отдела.")
            two.send(ro_mail, f"Только чтение {stamp}", "Письмо в ящик только для чтения.")
        if scene == "st-s528-take":
            got = sales.by_subject(subject)[0]
            who = [k for k in got["keywords"] if k.startswith("pm.taken.") and three.user.split("@")[0] in k]
            assert who, f"на сервере не взято: {got['keywords']}"
            print("сервер: взято в работу user3")
        if scene == "st-18-reply":
            got = two.wait(subject, two.boxes()["inbox"], seconds=30)
            assert got["from"][0]["email"] == sales_mail, got["from"]
            print("сервер: ответ от общего ящика")
            sent = sales.boxes()["sent"]
            assert sales.by_subject(subject, sent), "нет копии в «Отправленных» ящика"
            print("сервер: копия в «Отправленных» ящика")
        if scene == "st-18-flag":
            # Следующая сцена уже может перенести письмо в «Удалённые»: ищем во всём ящике.
            flagged = lambda: any("$flagged" in g["keywords"] for g in sales.by_subject(subject))
            wait_for(flagged, "нет флага в ящике группы", 10)
            print("сервер: флаг в ящике группы")
        if scene == "st-18-delete":
            trash = sales.boxes()["trash"]
            for _ in range(20):
                if sales.by_subject(subject, trash):
                    break
                time.sleep(0.5)
            assert sales.by_subject(subject, trash), "письмо не в «Удалённых» ящика"
            print("сервер: письмо в «Удалённых» ящика")

    def second(scene):
        if scene == "st-s528-steal":
            for _ in range(20):
                got = sales.by_subject(subject)[0]
                if [k for k in got["keywords"] if k.startswith("pm.taken.") and one.user.split("@")[0] in k]:
                    break
                time.sleep(0.5)
            print("сервер: забрал user1")

    w1 = ["st-18-tree", "st-18-push", "st-s528-take", "st-s528-lost", "st-18-reply", "st-18-ro", "st-18-flag", "st-18-delete"]
    together(lambda: play.run(w1, first, three.user, three.password, extra={"POSTMAN_STAND_PEER": one.user}),
             lambda: play.run(["st-s528-sees", "st-s528-steal"], second, one.user, one.password, data="shared-second", extra={"POSTMAN_STAND_PEER": three.user}))


def group_accounts(ctx):
    """S7-03б шаги 1–3, 6, 8: вторая учётная запись добавляется в окне с «Не хранить»: в диспетчер учётных данных ничего не пишется."""
    play = Play(ctx, "accounts")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    extra = {"POSTMAN_STAND_PEER": two.user, "POSTMAN_STAND_PEER_SECRET": two.password}
    one.send(two.user, f"Второй учётке {stamp}", "Письмо в ящик второй учётной записи.")
    one.send(two.user, f"Удалить у второй {stamp}", "Это письмо удаляется в окне.")
    two.wait(f"Удалить у второй {stamp}", two.boxes()["inbox"])
    event = Dav(two).new_event(f"Событие второй {stamp}")

    def on_scene(scene):
        if scene == "st-s703-letter":
            for _ in range(20):
                got = two.by_subject(f"Второй учётке {stamp}", two.boxes()["inbox"])[0]
                if "$seen" in got["keywords"]:
                    break
                time.sleep(0.5)
            assert "$seen" in got["keywords"], got
            print("сервер: прочитано у user2")
        if scene == "st-s703-move":
            inbox = two.boxes()["inbox"]
            for _ in range(20):
                if not two.by_subject(f"Второй учётке {stamp}", inbox):
                    break
                time.sleep(0.5)
            assert not two.by_subject(f"Второй учётке {stamp}", inbox), "письмо осталось во «Входящих»"
            print("сервер: перенесено у user2")
        if scene == "st-s703-delete":
            trash = two.boxes()["trash"]
            for _ in range(20):
                if two.by_subject(f"Удалить у второй {stamp}", trash):
                    break
                time.sleep(0.5)
            assert two.by_subject(f"Удалить у второй {stamp}", trash), "письмо не в «Удалённых»"
            print("сервер: удалено у user2")
        if scene == "st-s703-from":
            got = one.wait(f"От второй учётки {stamp}", one.boxes()["inbox"], seconds=30)
            assert got["from"][0]["email"] == two.user, got["from"]
            assert two.by_subject(f"От второй учётки {stamp}", two.boxes()["sent"]), "нет копии в «Отправленных» user2"
            print("сервер: письмо от user2, копия в его «Отправленных»")
        if scene == "st-s703-meeting":
            _, body = Dav(two).wait("VEVENT", f"Встреча обеих {stamp}")
            assert f"ORGANIZER" in body and two.user in body, body
            print("сервер: организатор user2")

    only = ["st-s703-wrong", "st-s703-same", "st-s703-add", "st-s703-letter", "st-s703-reply", "st-s703-move", "st-s703-delete", "st-s703-from",
            "st-s703-cal", "st-s703-meeting", "st-s703-search", "st-s703-remove"]
    try:
        play.run(only, on_scene, one.user, one.password, extra=extra)
    finally:
        Dav(two).delete(event)


def group_box(ctx):
    """Разделы 14 (шаги 2, 3, 4, 6, 7) и 34 (шаг 2). Пароль меняется у временной учётной записи, стенд возвращается в `finally`."""
    play = Play(ctx, "box")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    dav = Dav(one)
    two.send(one.user, f"Непрочитанное {stamp}", "Счётчик папки.")
    one.wait(f"Непрочитанное {stamp}", one.boxes()["inbox"])
    event = {}

    def on_scene(scene):
        if scene == "st-14-read":
            for _ in range(20):
                if "$seen" in one.by_subject(f"Непрочитанное {stamp}", one.boxes()["inbox"])[0]["keywords"]:
                    break
                time.sleep(0.5)
            assert "$seen" in one.by_subject(f"Непрочитанное {stamp}", one.boxes()["inbox"])[0]["keywords"]
        if scene == "st-14-unread":
            for _ in range(20):
                if "$seen" not in one.by_subject(f"Непрочитанное {stamp}", one.boxes()["inbox"])[0]["keywords"]:
                    break
                time.sleep(0.5)
            assert "$seen" not in one.by_subject(f"Непрочитанное {stamp}", one.boxes()["inbox"])[0]["keywords"]
            print("сервер: снова не прочитано")
            event["href"] = dav.new_event(f"Встреча стенда {stamp}")
        if scene == "st-14-week":
            dav.delete(event["href"])

    play.run(["st-14-read", "st-14-unread", "st-14-week", "st-14-week-gone", "st-34-to", "st-34-chip"], on_scene, one.user, one.password)

    admin = Admin()
    domain = one.user.split("@")[1]
    name, first, second = "drill" + secrets.token_hex(2), secrets.token_urlsafe(18), secrets.token_urlsafe(18)
    account = admin.temp_user(name, first)
    email = f"{name}@{domain}"
    try:
        temp = Mailbox(user=email, password=first)
        two.send(email, f"Первое письмо {stamp}", "Ящику нужно письмо.")
        temp.wait(f"Первое письмо {stamp}")

        def changed(scene):
            if scene == "live-sync":
                admin.new_password(account, second)
                print("стенд: пароль сменён при открытом окне")

        play.run(["live-sync", "st-14-password"], changed, email, first, data="box-temp")
        # Раздел 14, шаг 3: новый запуск со старым паролем: строка входа, тостов нет.
        play.run(["live-rejected"], lambda _: None, email, first, data="box-temp")
    finally:
        admin.destroy(account)

    def down(scene):
        if scene == "live-sync":
            subprocess.run(["docker", "stop", "-t", "1", CONTAINER], check=True, capture_output=True)
            print("сервер остановлен")
        if scene == "live-offline":
            subprocess.run(["docker", "start", CONTAINER], check=True, capture_output=True)
            print("сервер запущен")

    try:
        # Раздел 14, шаг 7: строка состояния и ошибка синхронизации по-английски.
        play.run(["live-sync", "live-offline", "live-online"], down, one.user, one.password, data="box-en", lang="en")
    finally:
        subprocess.run(["docker", "start", CONTAINER], capture_output=True)
        wait_stand()


class Imap:
    """Другой клиент по IMAP (вместо Thunderbird): сертификат стенда самоподписанный."""

    def __init__(self, box):
        host, port = os.environ["POSTMAN_STAND_IMAP"].split(":")
        context = ssl.create_default_context()
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        self.link = imaplib.IMAP4_SSL(host, int(port), ssl_context=context)
        self.link.login(box.user, box.password)

    def uids(self, folder, text):
        self.link.select(f'"{folder}"')
        self.link.literal = text.encode()
        typ, data = self.link.uid("SEARCH", "CHARSET", "UTF-8", "SUBJECT")
        assert typ == "OK", data
        return data[0].split()

    def flags(self, folder, text):
        found = self.uids(folder, text)
        assert found, f"в {folder} нет письма «{text}»"
        typ, data = self.link.uid("FETCH", found[-1], "(FLAGS)")
        return data[0].decode()

    def store(self, folder, text, how, flag):
        found = self.uids(folder, text)
        assert found, f"в {folder} нет письма «{text}»"
        self.link.uid("STORE", found[-1], how, flag)

    def close(self):
        self.link.logout()


class Card:
    """Адресная книга по CardDAV."""

    def __init__(self, box):
        self.dav = Dav(box)
        self.path = f"/dav/card/{urllib.parse.quote(box.user, safe='')}/default/"

    def has(self, text):
        query = '<c:addressbook-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop><c:address-data/></d:prop></c:addressbook-query>'
        return text in self.dav.request("REPORT", self.path, query)


def wait_for(check, what, seconds=20):
    for _ in range(seconds * 2):
        if check():
            return
        time.sleep(0.5)
    raise AssertionError(what)


def group_imap(ctx):
    """S7-04 шаги 1–8: основная учётная запись — IMAP и SMTP; свидетель — запросы IMAP."""
    play = Play(ctx, "imap")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    two.send(one.user, f"С вложением {stamp}", "Письмо для папок IMAP.")
    two.send_rich(one.user, f"Письмо с оформлением {stamp}", "Письмо с оформлением: простой текст.",
                  '<p>Письмо с <b>жирное слово</b> внутри.</p><img src="https://example.test/pixel.png" alt="внешняя картинка">', "отчёт.txt", "Вложение: строка текста.\n")
    two.send(one.user, f"Беседа IMAP {stamp}", "Начало беседы.")
    one.wait(f"С вложением {stamp}", one.boxes()["inbox"])
    one.wait(f"Письмо с оформлением {stamp}", one.boxes()["inbox"])
    one.wait(f"Беседа IMAP {stamp}", one.boxes()["inbox"])
    box = Imap(one)
    host = os.environ["POSTMAN_STAND_IMAP"]
    smtp = os.environ["POSTMAN_STAND_SMTP"]
    url = f"imaps://{host}?smtp={smtp}"

    def on_scene(scene):
        if scene == "st-s704-send":
            wait_for(lambda: box.uids("Sent Items", stamp), "нет копии в Sent Items")
            print("сервер: копия в Sent Items")
        if scene == "st-s704-flags":
            def marked():
                flags = box.flags("INBOX", f"Себе с вложением {stamp}")
                return "\\Seen" in flags and "\\Flagged" in flags and "pm.cat." in flags
            wait_for(marked, "флаги не дошли до сервера")
            print("сервер: прочитано, флаг, категория")
            box.store("INBOX", f"Себе с вложением {stamp}", "-FLAGS", "\\Flagged")
            print("другой клиент: флаг снят")
        if scene == "st-s704-move":
            wait_for(lambda: not [u for u in box.uids("INBOX", f"Себе с вложением {stamp}")], "письмо осталось во Inbox")
            print("сервер: перенесено")
        if scene == "st-s704-delete":
            wait_for(lambda: box.uids("Deleted Items", f"С вложением {stamp}"), "нет в Deleted Items")
            print("сервер: в Deleted Items")
        if scene == "st-s704-purge":
            wait_for(lambda: not box.uids("Deleted Items", f"С вложением {stamp}"), "письмо осталось в Deleted Items")
            print("сервер: удалено насовсем")
        if scene == "st-s704-draft":
            wait_for(lambda: len(box.uids("Drafts", f"Черновик IMAP {stamp}")) == 1, "в Drafts не одна копия")
            print("сервер: одна копия в Drafts")
        if scene == "st-s704-undo":
            # Окно «Отменить» — 10 с: отменённое письмо не должно прийти и после него.
            time.sleep(12)
            assert not two.by_subject(f"Отмена IMAP {stamp}"), "отменённое письмо ушло"
            print("сервер: отменённое письмо не ушло")
        if scene == "st-s704-resend":
            two.wait(f"Отмена IMAP {stamp}", two.boxes()["inbox"], seconds=30)
            print("сервер: без отмены письмо ушло")
        if scene == "st-s704-thread":
            two.wait(f"Беседа IMAP {stamp}", two.boxes()["inbox"], seconds=30)
            print("сервер: ответ у user2")

    extra = {"POSTMAN_STAND_PEER": two.user}
    only = ["st-s704-tree", "st-s704-open", "st-s704-send", "st-s704-flags", "st-s704-flag-back", "st-s704-move", "st-s704-delete", "st-s704-purge",
            "st-s704-draft", "st-s704-html", "st-s704-preview", "st-s704-undo", "st-s704-resend", "st-s704-thread"]
    try:
        play.run(only, on_scene, one.user, one.password, url=url, extra=extra)
        # Шаг 9: письмо в окне «Отменить», окно закрыто: письмо уходит при закрытии.
        play.run(["st-s704-quit"], lambda _: None, one.user, one.password, url=url, extra=extra, hidden=["st-s704-quit-close"])
        two.wait(f"Закрытие IMAP {stamp}", two.boxes()["inbox"], seconds=20)
        print("сервер: письмо ушло при закрытии", flush=True)
    finally:
        box.close()
    # Шаг 8: большой ящик, письмо за первыми 500: тема — из конца ящика.
    big = Mailbox("")
    got = big.call([
        ["Email/query", {"accountId": big.account, "sort": [{"property": "receivedAt", "isAscending": True}], "limit": 1}, "0"],
        ["Email/get", {"accountId": big.account, "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}, "properties": ["subject"]}, "1"],
    ])
    far = got[1][1]["list"][0]
    word = far["subject"]
    play.run(["st-s704-far"], lambda _: None, big.user, big.password, url=url, data="imap-big", extra={"POSTMAN_STAND_WORD": word})
    # Шаг 10: фильтр «Непрочитанные» по всем папкам: письмо из конца ящика — единственное непрочитанное, которого нет в первых 500.
    far_id = big.call([["Email/query", {"accountId": big.account, "sort": [{"property": "receivedAt", "isAscending": True}], "limit": 1}, "0"]])[0][1]["ids"][0]
    unread = big.call([["Email/query", {"accountId": big.account, "filter": {"notKeyword": "$seen"}, "calculateTotal": True, "limit": 0}, "0"]])[0][1]["total"]
    print("непрочитанных в большом ящике:", unread)
    big.call([["Email/set", {"accountId": big.account, "update": {far_id: {"keywords/$seen": None}}}, "0"]])
    try:
        play.run(["st-s704-filter"], lambda _: None, big.user, big.password, url=url, data="imap-big", extra={"POSTMAN_STAND_WORD": word})
    finally:
        big.call([["Email/set", {"accountId": big.account, "update": {far_id: {"keywords/$seen": True}}}, "0"]])


def group_dav(ctx):
    """S7-05 шаги 1–4, 6–8: IMAP и CalDAV/CardDAV; свидетель — запросы CalDAV и CardDAV."""
    play = Play(ctx, "dav")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    dav, peer = Dav(one), Dav(two)
    card = Card(one)
    series = dav.new_event(f"Серия стенда {stamp}", "FREQ=DAILY;COUNT=5")
    host = os.environ["POSTMAN_STAND_IMAP"]
    url = f"imaps://{host}?smtp={os.environ['POSTMAN_STAND_SMTP']}&dav=https://{host.split(':')[0]}"
    meeting = f"Встреча CalDAV {stamp}"
    first, edited, server, order = f"Задача стенда {stamp}", f"Задача правка {stamp}", f"С сервера {stamp}", f"Поручение стенда {stamp}"

    def on_scene(scene):
        if scene == "st-s705-meeting":
            href, body = peer.wait("VEVENT", meeting, seconds=30)
            print("сервер: приглашение в календаре user2")
            two.wait(f"Invitation: {meeting}", two.boxes()["inbox"], seconds=30)
            print("сервер: письмо-приглашение у user2")
            peer.put(href, body.replace("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED"))
            print("user2 принял приглашение")
        if scene == "st-s705-accepted":
            one.wait(f"Accepted: {meeting}", one.boxes()["inbox"], seconds=30)
            print("сервер: письмо «Accepted» у user1")
        if scene == "st-s705-contact":
            wait_for(lambda: card.has(f"{stamp}@example.test"), "нет карточки в CardDAV")
            print("сервер: карточка в CardDAV")

        if scene == "st-s705-task":
            dav.wait("VTODO", first)
            print("сервер: VTODO из Postman")
        if scene == "st-s705-task-due":
            for _ in range(20):
                href, body = dav.wait("VTODO", first)
                if "DUE" in body:
                    break
                time.sleep(0.5)
            assert "DUE" in body, body
            print("сервер: срок")
            dav.put(href, body.replace(first, edited))
            dav.new_todo(server)
            print("другой клиент: правка и новая задача")
        if scene == "st-s705-task-done":
            for _ in range(30):
                _, body = dav.wait("VTODO", edited)
                if "STATUS:COMPLETED" in body:
                    break
                time.sleep(0.5)
            assert "STATUS:COMPLETED" in body, body
            print("сервер: STATUS:COMPLETED")
        if scene == "st-s705-task-gone":
            dav.gone("VTODO", server)
            print("сервер: задачи нет")
        if scene == "st-s705-order":
            # Что сервер не рассылает задачу с `SCHEDULE-AGENT=CLIENT` сам, сверяет `caldav_task_round_trip`: здесь письмо уже в пути.
            got = two.wait(f"Поручение: {order}", two.boxes()["inbox"], seconds=30)
            assert got["from"][0]["email"] == one.user, got["from"]
            print("сервер: письмо-поручение от user1")
            peer.wait("VTODO", order, seconds=30)
            print("сервер: копию у user2 положило письмо iMIP")
            # Окно user2 — после письма: оно ждёт поручение не дольше времени шага сцены.
            play.run(["st-s705-order-got"], lambda _: None, two.user, two.password, url=url, data="dav-second")

    only = ["st-s705-rail", "st-s705-week", "st-s705-search", "st-s705-meeting", "st-s705-accepted", "st-s705-contact", "st-s705-suggest",
            "st-s705-task", "st-s705-task-due", "st-s705-task-back", "st-s705-task-done", "st-s705-task-gone", "st-s705-order", "st-s705-order-sent"]
    try:
        play.run(only, on_scene, one.user, one.password, url=url, extra={"POSTMAN_STAND_PEER": two.user})
        # Шаг 5: занятость user2 у user1: пока календарь не открыт — «нет данных».
        busy = peer.new_event(f"Занятость стенда {stamp}")
        try:
            play.run(["st-s705-nobusy"], lambda _: None, one.user, one.password, url=url, extra={"POSTMAN_STAND_PEER": two.user})
            print("окно: занятости user2 нет: «нет данных»")
            peer.share(dav.principal())
            print("другой клиент: user2 открыл календарь")
            play.run(["st-s705-busy"], lambda _: None, one.user, one.password, url=url, extra={"POSTMAN_STAND_PEER": two.user})
            print("окно: занятость user2 видна")
        finally:
            peer.unshare()
            peer.delete(busy)
    finally:
        dav.delete(series)


def group_access(ctx):
    """S8-43 шаги 1–5: A — user1, B — user2. Окна идут по очереди: каждый запуск — новая сверка с сервером."""
    play = Play(ctx, "access")
    one, two = Mailbox(1), Mailbox(2)
    dav = Dav(one)
    event = dav.new_event(f"Занятость стенда {ctx.stamp}")
    to_b, to_a = {"POSTMAN_STAND_PEER": two.user}, {"POSTMAN_STAND_PEER": one.user}
    a = lambda scenes: play.run(scenes, lambda _: None, one.user, one.password, extra=to_b)
    b = lambda scenes: play.run(scenes, lambda _: None, two.user, two.password, data="access-b", extra=to_a)
    try:
        a(["st-s843-share"])
        b(["st-s843-busy"])
        a(["st-s843-level"])
        b(["st-s843-seen"])
        a(["st-s843-again"])
        a(["st-s843-kept", "st-s843-close"])
        b(["st-s843-none"])
    finally:
        dav.delete(event)


def fill(box, count, word):
    """`count` писем во «Входящие» партиями `Email/import`."""
    inbox = box.boxes()["inbox"]
    done, part = 0, 500
    while done < count:
        n = min(part, count - done)
        create, blobs = {}, {}
        for i in range(n):
            text = (f"From: Коллега <peer@example.test>\r\nTo: {box.user}\r\nSubject: {word} {done + i}\r\n"
                    f"Message-ID: <{word}-{done + i}@example.test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nПисьмо номер {done + i}.\r\n")
            blobs[f"b{i}"] = {"data:asText": text}
            create[f"m{i}"] = {"blobId": f"#b{i}", "mailboxIds": {inbox: True}, "keywords": {"$seen": True}}
        got = box.call([
            ["Blob/upload", {"accountId": box.account, "create": {k: {"data": [v], "type": "message/rfc822"} for k, v in blobs.items()}}, "0"],
            ["Email/import", {"accountId": box.account, "emails": create}, "1"],
        ], [CORE, MAIL, "urn:ietf:params:jmap:blob"])
        assert len(got[1][1].get("created") or {}) == n, str(got[1][1])[:300]
        done += n


def group_move(ctx):
    """S8-15 шаги 3–5, S8-16 шаги 1–3, 5–7. Обе учётные записи временные: письма уходят из ящика-источника. Цель добавляется в окне с «Не хранить»."""
    play = Play(ctx, "move")
    admin = Admin()
    domain = os.environ["POSTMAN_STAND_USER1"].split("@")[1]
    names = ["dsrc" + secrets.token_hex(2), "ddst" + secrets.token_hex(2)]
    secret = [secrets.token_urlsafe(18), secrets.token_urlsafe(18)]
    ids = [admin.temp_user(n, p) for n, p in zip(names, secret)]
    source, target = [Mailbox(user=f"{n}@{domain}", password=p) for n, p in zip(names, secret)]
    extra = {"POSTMAN_STAND_PEER": target.user, "POSTMAN_STAND_PEER_SECRET": target.password}
    # Меньше страницы загрузки: Ctrl+A выделяет то, что в кэше, письма за ней остались бы у источника.
    count = int(os.environ.get("POSTMAN_STAND_MOVE", "400"))

    def total(box):
        got = box.call([["Email/query", {"accountId": box.account, "filter": {"inMailbox": box.boxes()["inbox"]}, "calculateTotal": True, "limit": 1}, "0"]])
        return got[0][1]["total"]

    def distinct(box):
        seen, position = [], 0
        while True:
            got = box.call([
                ["Email/query", {"accountId": box.account, "filter": {"inMailbox": box.boxes()["inbox"]}, "position": position, "limit": 500}, "0"],
                ["Email/get", {"accountId": box.account, "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}, "properties": ["messageId"]}, "1"],
            ])
            rows = got[1][1]["list"]
            seen += [m for r in rows for m in r["messageId"]]
            position += len(rows)
            if len(rows) < 500:
                return len(seen), len(set(seen))

    def run(scenes, on_scene=lambda _: None):
        # Пароль цели в профиле не хранится («Не хранить»): каждый запуск добавляет её заново.
        play.run(["st-s703-add"] + scenes, on_scene, source.user, source.password, extra=extra)

    started = {}

    def first(scene):
        if scene == "st-s815-menu":
            started["drag"] = time.time()
        if scene == "st-s815-target":
            print(f"список цели показал письма за {time.time() - started['drag']:.1f} с", flush=True)

    def second(scene):
        if scene == "st-s816-stop-no":
            print("сервер цели посреди выгрузки:", total(target), flush=True)

    try:
        started["fill"] = time.time()
        fill(source, count, "move" + ctx.stamp)
        print(f"письма источника: {count} за {time.time() - started['fill']:.0f} с", flush=True)
        run(["st-s816-local", "st-s815-letter", "st-s815-menu", "st-s815-drag", "st-s815-target", "st-s816-ask"], first)
        # Сцены начинаются со строк списка: у источника всегда есть письмо.
        fill(source, 1, "keep" + ctx.stamp)
        # Шаг 4, выход посреди выгрузки, не здесь: цель без сохранённого пароля появляется уже после старта задания, команды
        # ядра к ней не доходят, и задание стоит. Шаг 7 — в том же запуске.
        run(["st-s816-kept", "st-s816-pause", "st-s816-resume", "st-s816-stop-no", "st-s816-done"], second)
        wait_for(lambda: total(source) == 1, "у источника письма остались", 60)
        have, unique = distinct(target)
        assert have == unique, f"дубли на сервере цели: {have} писем, разных {unique}"
        print(f"сервер: у цели {have} писем без дублей, у источника только новое письмо", flush=True)
        fill(source, 300, "back" + ctx.stamp)
        run(["st-s816-start", "st-s816-return"])
        # Уже выгруженные до остановки остаются у цели, остальные — снова у источника, ни одно не потеряно.
        wait_for(lambda: total(source) + total(target) == have + 301, "письма потерялись при возврате", 60)
        print(f"сервер: у источника {total(source)}, у цели {total(target)}: письма на месте", flush=True)
        run(["st-s816-start-2", "st-s816-keep"])
        fill(source, 300, "full" + ctx.stamp)
        used = admin.accounts()[target.user]["usedDiskQuota"]
        admin.quota(target.user, used + 3000)
        run(["st-s816-start-3", "st-s816-full", "st-s816-local-rest"])
    finally:
        for account in ids:
            admin.destroy(account)


def together(*runs):
    """Два окна сразу: сцены ждут друг друга условиями `Until`, сервер — посредник."""
    with concurrent.futures.ThreadPoolExecutor(len(runs)) as pool:
        jobs = [pool.submit(r) for r in runs]
        for job in concurrent.futures.as_completed(jobs):
            # Одно окно сорвалось: второе ждёт его условием `Until` — ждать незачем.
            if job.exception():
                for app in list(RUNNING):
                    app.kill()
        for job in jobs:
            job.result()


def wait_stand():
    for _ in range(60):
        try:
            Mailbox(1)
            return
        except (urllib.error.URLError, ConnectionError, OSError):
            time.sleep(1)
    raise AssertionError("стенд не поднялся")


class Ctx:
    def __init__(self, exe, shots, data):
        self.exe, self.shots, self.data = exe, shots, data
        self.stamp = "r" + secrets.token_hex(3)


GROUPS = {"live": group_live, "rules": group_rules, "vacation": group_vacation, "tasks": group_tasks, "card": group_card, "full": group_full, "own": group_own, "files": group_files, "forward": group_forward, "more": group_more, "deleg": group_deleg,
          "shared": group_shared, "accounts": group_accounts, "box": group_box, "imap": group_imap, "dav": group_dav, "access": group_access, "move": group_move}


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    exe, shots, data = sys.argv[1:4]
    ctx = Ctx(exe, shots, data)
    names = sys.argv[4:] or list(GROUPS)
    failed = []
    for name in names:
        print("== группа", name, flush=True)
        # Своя метка: задачи и поручения с теми же названиями у групп — в одном ящике.
        ctx.stamp = "r" + secrets.token_hex(3)
        try:
            GROUPS[name](ctx)
            print("== готово", name, flush=True)
        except Exception as e:
            print("== ПАДЕНИЕ", name, str(e)[:400], flush=True)
            traceback.print_exc(limit=3)
            failed.append(name)
    print("упало групп:", len(failed), " ".join(failed))
    sys.exit(1 if failed else 0)


main()
