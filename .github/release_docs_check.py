"""Гейт выпуска: полная сверка документации (решение 2026-11-12 «release-docs-gate»).

python release_docs_check.py <версия>
Нужен docs/releases/<версия>.md: каждый документ — строка «- [x] `путь` — итог». Документы — все *.md в корне и в
docs/ (без подкаталогов), scripts/*/README.txt и .github/release-notes.md; новый документ попадает в список сам.
Непроставленная отметка, пропущенный или лишний (несуществующий) путь — выпуск красный.
"""

import glob
import re
import sys

ITEM = re.compile(r"^- \[( |x|X)\] `([^`]+)`(.*)$")


def required() -> set[str]:
    paths = set(glob.glob("*.md")) | set(glob.glob("docs/*.md")) | set(glob.glob("scripts/*/README.txt"))
    paths.add(".github/release-notes.md")
    return paths


def check(version: str, text: str, need: set[str]) -> list[str]:
    errors: list[str] = []
    seen: set[str] = set()
    for n, line in enumerate(text.splitlines(), 1):
        m = ITEM.match(line.strip())
        if not m:
            continue
        mark, path, rest = m.groups()
        seen.add(path)
        if mark == " ":
            errors.append(f"строка {n}: `{path}` не сверен")
        elif not rest.strip(" —-:"):
            errors.append(f"строка {n}: `{path}` — нет итога (что обновлено или «без изменений»)")
        if path not in need:
            errors.append(f"строка {n}: `{path}` — такого документа нет")
    for path in sorted(need - seen):
        errors.append(f"`{path}` нет в сверке")
    if f"# Документация к выпуску {version}" not in text:
        errors.append(f"заголовок должен быть «# Документация к выпуску {version}»")
    return errors


def main() -> int:
    version = sys.argv[1]
    path = f"docs/releases/{version}.md"
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except FileNotFoundError:
        print(f"::error::нет {path}: перед тегом — полная сверка документации (docs/DEV.md «Выпуск», шаблон docs/releases/TEMPLATE.md)")
        return 1
    errors = check(version, text, required())
    for e in errors:
        print(f"::error file={path}::{e}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
