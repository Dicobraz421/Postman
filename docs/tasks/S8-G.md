# S8-G: перегенерация `docs/PARITY.md` после новых команд S8

lane: docs-gen
status: done
deps: [S8-16, S8-27, S8-32]
class: A
files: [docs/PARITY.md]
read: []
checks: [nextest]
manual: no

> Новые команды S8: перенос между учётками и действия задания переноса (S8-15, S8-16), `mail.export`,
> `folder.export`, `account.export` (S8-32), раздел «Просмотр вложений» (S8-27).

## Сделать
- `POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui parity` от свежего main, одной волной в день.

## Приёмка
- `parity` зелёный без переменной.
