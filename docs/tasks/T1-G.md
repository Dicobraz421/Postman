# T1-G: перегенерация `docs/PARITY.md` после новых команд T1

lane: docs-gen
status: todo
deps: [T1-04д, T1-17, T1-23]
class: A
files: [docs/PARITY.md]
read: []
checks: [nextest]
manual: no

> Новые команды ручной проверки 1: `folder.new`, `folder.rename` (T1-04д), `mail.forward_attached` (T1-17), действия
> контакта (T1-23).

## Сделать
- От свежего `main` после волн app-gpui: `POSTMAN_UPDATE_DOCS=1 cargo test -p postman-gpui parity`. Одна волна в день.

## Приёмка
- `parity_doc_is_current` зелёный на `main`.
