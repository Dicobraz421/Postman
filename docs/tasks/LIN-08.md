# LIN-08: кнопки окна по настройке ОС; при кнопках слева — раскладка macOS

lane: app-gpui
status: todo
deps: [LIN-02, MAC-DES]
class: A
files: [app-gpui/src/chrome.rs, app-gpui/src/bar.rs, app-gpui/src/tabs.rs, docs/manual-tests/LIN-08.md]
read: [docs/DESIGN.md#linux, docs/DESIGN.md#macos, docs/tasks/MAC-DES.md]
checks: [clippy, nextest, ci-linux, screens:list]
manual: docs/manual-tests/LIN-08.md

> Решение владельца 2026-11-14 (сессия дизайна). Макет: `MainLinuxLeft`.

## Сделать
1. Читать `button-layout` (XDG Settings portal, `org.gnome.desktop.wm.preferences`), следить за сменой; разбор строки
   «close,minimize:maximize» — разбор с тестом. Нет портала или ключа — справа, все три.
2. Кнопки справа — как сейчас. Слева — раскладка MAC-DES (общий код, не копия): кнопки окна вместо «светофора»,
   разделы над деревом, «Новое письмо» над списком; шестерёнка — справа в заголовке.
3. Полный экран — раскладка Windows без кнопок окна.

## Приёмка
- Тест разбора `button-layout`; снимок `list` под Xvfb с кнопками слева — как макет.
