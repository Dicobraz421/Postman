"""Вторая сторона живых сцен (`app-gpui/src/drill_live.rs`), стенд — локальный контейнер Stalwart.

Запускает `postman.exe --screens … --live` от user1 и по строкам `screen <сцена>` играет коллегу: письма от user2,
остановка и запуск сервера. Сверяет состояние на сервере запросами JMAP. Пароли — только из окружения.

  python peer.py <postman.exe> <каталог снимков> <каталог данных>

Окружение: POSTMAN_STAND_URL, POSTMAN_STAND_USER1..2, POSTMAN_STAND_PASSWORD1..2, POSTMAN_STAND_USER, POSTMAN_STAND_PASSWORD.
"""

import base64
import json
import os
import subprocess
import sys
import time
import urllib.request

CONTAINER = "postman-stand"
URL = os.environ["POSTMAN_STAND_URL"]
USING = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"]


def http(user, password, path, body=None):
    req = urllib.request.Request(URL + path, data=None if body is None else json.dumps(body).encode())
    req.add_header("Authorization", "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode())
    req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode())


class Mailbox:
    def __init__(self, n):
        self.user = os.environ[f"POSTMAN_STAND_USER{n}"]
        self.password = os.environ[f"POSTMAN_STAND_PASSWORD{n}"]
        session = http(self.user, self.password, "/jmap/session")
        self.account = session["primaryAccounts"]["urn:ietf:params:jmap:mail"]

    def call(self, calls):
        body = {"using": USING, "methodCalls": calls}
        return http(self.user, self.password, "/jmap", body)["methodResponses"]

    def boxes(self):
        got = self.call([["Mailbox/get", {"accountId": self.account, "ids": None, "properties": ["role"]}, "0"]])
        return {m.get("role"): m["id"] for m in got[0][1]["list"]}

    def send(self, to, subject, text):
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
        got = self.call([
            ["Email/set", {"accountId": self.account, "create": {"m": email}}, "0"],
            ["EmailSubmission/set", {"accountId": self.account, "create": {"s": submission}}, "1"],
        ])
        assert "s" in got[1][1].get("created", {}), got

    def top(self):
        got = self.call([
            ["Email/query", {"accountId": self.account, "sort": [{"property": "receivedAt", "isAscending": False}], "limit": 1}, "0"],
            ["Email/get", {"accountId": self.account, "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}, "properties": ["subject", "keywords"]}, "1"],
        ])
        return got[1][1]["list"][0]


def wait_arrived(box, subject):
    for _ in range(40):
        if subject in (box.top().get("subject") or ""):
            return
        time.sleep(0.5)
    sys.exit(f"письмо «{subject}» не пришло")


def main():
    exe, shots, data = sys.argv[1:4]
    one, two = Mailbox(1), Mailbox(2)
    stamp = str(int(time.time()))
    two.send(one.user, f"Договор {stamp}", "Письмо по договору о сроках поставки оборудования. It mentions a quarterly budget.")
    wait_arrived(one, stamp)

    only = "live-sync,live-read,live-push,live-search-1,live-search-2,live-offline,live-online"
    env = dict(os.environ, LOCALAPPDATA=data, POSTMAN_SCREENS_ONLY=only, POSTMAN_URL=URL, POSTMAN_USER=one.user, POSTMAN_PASSWORD=one.password)
    app = subprocess.Popen([exe, "--screens", shots, "--live", "--lang", "ru"], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
    seen = []
    for line in app.stdout:
        if line.startswith("screens:"):
            print(line.strip())
        if not line.startswith("screen "):
            continue
        scene = line.split()[1]
        seen.append(scene)
        print("сцена", scene, flush=True)
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
    code = app.wait()
    print("код", code, "сцены:", " ".join(seen))
    sys.exit(code)


main()
