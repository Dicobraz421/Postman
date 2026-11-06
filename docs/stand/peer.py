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
import json
import os
import re
import secrets
import ssl
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request

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

    def send(self, to, subject, text, keep=False):
        """Письмо через EmailSubmission; `keep` — копия в «Отправленных», иначе письмо остаётся в «Черновиках»."""
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
        submission = {"identityId": ident, "emailId": "#m"}
        arg = {"accountId": self.account, "create": {"s": submission}}
        if keep:
            arg["onSuccessUpdateEmail"] = {"#s": {f"mailboxIds/{boxes['drafts']}": None, f"mailboxIds/{boxes['sent']}": True, "keywords/$draft": None}}
        got = self.call([
            ["Email/set", {"accountId": self.account, "create": {"m": email}}, "0"],
            ["EmailSubmission/set", arg, "1"],
        ])
        assert "s" in got[1][1].get("created", {}), got

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


def upload(box, text):
    path = box.session["uploadUrl"].replace("{accountId}", box.account)
    path = path[path.index("/jmap"):]
    req = urllib.request.Request(URL + path, data=text.encode(), method="POST")
    req.add_header("Authorization", "Basic " + base64.b64encode(f"{box.user}:{box.password}".encode()).decode())
    req.add_header("Content-Type", "application/sieve")
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

    def new_event(self, title, rule=None):
        uid = "peer-" + secrets.token_hex(6)
        now = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        start = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime(time.time() + 3600))
        end = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime(time.time() + 5400))
        ics = ("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//postman stand peer//RU\r\nBEGIN:VEVENT\r\n"
               f"UID:{uid}\r\nDTSTAMP:{now}\r\nDTSTART:{start}\r\nDTEND:{end}\r\nSUMMARY:{title}\r\n" + (f"RRULE:{rule}\r\n" if rule else "") + "END:VEVENT\r\nEND:VCALENDAR\r\n")
        self.put(self.path + uid + ".ics", ics)
        return self.path + uid + ".ics"

    def delete(self, href):
        self.request("DELETE", href, None, depth="0")

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

    def run(self, scenes, on_scene, user, password, url=URL, data=None, lang="ru", extra=None):
        ctx = self.ctx
        place = os.path.join(ctx.data, data or self.group)
        os.makedirs(place, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("STAND_", "POSTMAN_STAND_"))}
        env.update(LOCALAPPDATA=place, POSTMAN_SCREENS_ONLY=",".join(scenes), POSTMAN_URL=url, POSTMAN_USER=user, POSTMAN_PASSWORD=password,
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
    play.run(main, on_scene, one.user, one.password)


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
            for _ in range(20):
                got = sales.by_subject(subject, sales.boxes()["inbox"])[0]
                if "$flagged" in got["keywords"]:
                    break
                time.sleep(0.5)
            assert "$flagged" in got["keywords"], got["keywords"]
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
    one.wait(f"С вложением {stamp}", one.boxes()["inbox"])
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

    extra = {"POSTMAN_STAND_PEER": two.user}
    only = ["st-s704-tree", "st-s704-open", "st-s704-send", "st-s704-flags", "st-s704-flag-back", "st-s704-move", "st-s704-delete", "st-s704-purge",
            "st-s704-draft"]
    try:
        play.run(only, on_scene, one.user, one.password, url=url, extra=extra)
    finally:
        box.close()
    # Шаг 8: большой ящик, письмо за первыми 500: тема — из конца ящика.
    big = Mailbox("")
    got = big.call([
        ["Email/query", {"accountId": big.account, "sort": [{"property": "receivedAt", "isAscending": True}], "limit": 1}, "0"],
        ["Email/get", {"accountId": big.account, "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}, "properties": ["subject"]}, "1"],
    ])
    word = got[1][1]["list"][0]["subject"]
    play.run(["st-s704-far"], lambda _: None, big.user, big.password, url=url, data="imap-big", extra={"POSTMAN_STAND_WORD": word})


def group_dav(ctx):
    """S7-05 шаги 1–4, 8: IMAP и CalDAV/CardDAV; свидетель — запросы CalDAV и CardDAV."""
    play = Play(ctx, "dav")
    one, two = Mailbox(1), Mailbox(2)
    stamp = ctx.stamp
    dav, peer = Dav(one), Dav(two)
    card = Card(one)
    series = dav.new_event(f"Серия стенда {stamp}", "FREQ=DAILY;COUNT=5")
    host = os.environ["POSTMAN_STAND_IMAP"]
    url = f"imaps://{host}?smtp={os.environ['POSTMAN_STAND_SMTP']}&dav=https://{host.split(':')[0]}"
    meeting = f"Встреча CalDAV {stamp}"

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

    only = ["st-s705-rail", "st-s705-week", "st-s705-search", "st-s705-meeting", "st-s705-accepted", "st-s705-contact", "st-s705-suggest"]
    try:
        play.run(only, on_scene, one.user, one.password, url=url, extra={"POSTMAN_STAND_PEER": two.user})
    finally:
        dav.delete(series)


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


GROUPS = {"live": group_live, "rules": group_rules, "vacation": group_vacation, "tasks": group_tasks, "deleg": group_deleg,
          "shared": group_shared, "accounts": group_accounts, "box": group_box, "imap": group_imap, "dav": group_dav}


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    exe, shots, data = sys.argv[1:4]
    ctx = Ctx(exe, shots, data)
    names = sys.argv[4:] or list(GROUPS)
    failed = []
    for name in names:
        print("== группа", name, flush=True)
        try:
            GROUPS[name](ctx)
            print("== готово", name, flush=True)
        except (AssertionError, urllib.error.URLError, KeyError, OSError) as e:
            print("== ПАДЕНИЕ", name, str(e)[:400], flush=True)
            traceback.print_exc(limit=3)
            failed.append(name)
    print("упало групп:", len(failed), " ".join(failed))
    sys.exit(1 if failed else 0)


main()
