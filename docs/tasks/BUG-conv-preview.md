# BUG-conv-preview: предпросмотр «Бесед» не меняется

lane: app-gpui
status: done
class: B
files: [app-gpui/src/settings_tab.rs]
checks: [clippy, nextest, screens]
manual: docs/manual-tests/BUG-beta7.md

> Владелец (macOS, 0.1.0-beta.5): «Настройки → Чтение → Беседы» — карточка «Предпросмотр» не меняется при выборе варианта.

## Причина
Схема в карточке рисовалась только по «Панели чтения» (`pane_scheme`), вид беседы в ней не участвовал.

## Исправление
`reading_scheme(pane, conversation)`: лента — письма подряд, стопка — свёрнутые заголовки над открытым письмом,
«как Outlook» — беседа деревом в списке. Тест — снимок `settings-reading`.
