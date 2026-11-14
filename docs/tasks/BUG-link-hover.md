# BUG-link-hover: адрес ссылки при наведении висит бесконечно

lane: app-gpui
status: done
class: B
files: [app-gpui/src/ir_view.rs]
checks: [clippy, nextest]
manual: docs/manual-tests/BUG-beta7.md

> Владелец (macOS, 0.1.0-beta.5): плашка адреса ссылки внизу панели чтения не уходит.

## Причина
`InteractiveText::on_hover` в gpui вызывается, только пока курсор над текстом: ухода с абзаца (и из окна) не
сообщает, адрес оставался.

## Исправление
Уход с абзаца или картинки-ссылки снимает адрес (`on_hover` обёртки); адрес держится не дольше `LINK_SHOWN` (4 с),
как строка состояния браузера. Тест `hovered_link_goes_away`.
