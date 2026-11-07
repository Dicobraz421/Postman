# Значки Fluent UI System Icons

Набор интерфейса Postman (ADR 0008, `docs/DESIGN.md` → «Значки»): начертание Regular 20. Контуры взяты из утверждённых
макетов «✓ Главное окно» (`MainNav_title`, `MainIcoFluent`); набор дополняется вручную и только нужными файлами. Шрифтом
ОС остаются кнопки окна и значки, которых здесь нет. Таблица в коде — `app-gpui/src/icons.rs`, тест
`set_matches_the_folder` сверяет её с этим каталогом.

Значок приложения — не отсюда: `app-gpui/res/postman.svg`.

| Файл | Где |
|---|---|
| `mail.svg` | раздел «Почта», заголовок своей учётной записи в дереве |
| `calendar.svg` | раздел «Календарь» |
| `people.svg` | раздел «Контакты», «Взять в работу», заголовок общего ящика в дереве |
| `tasks.svg` | раздел «Задачи» |
| `settings.svg` | шестерёнка «Настройки» |
| `compose.svg` | «Новое письмо» |
| `reply.svg` | «Ответить» |
| `reply-all.svg` | «Ответить всем» |
| `forward.svg` | «Переслать» |
| `delete.svg` | «Удалить», папка «Удалённые» |
| `move.svg` | «Переместить» |
| `tag.svg` | «Категория» |
| `flag.svg` | «Срок» |
| `clock.svg` | «Отложить» |
| `search.svg` | поле поиска, 18 px |
| `view.svg` | «Вид», 16 px; контур макета `MainIcoFluent` (не из набора Fluent), сетка 20 × 20 |
| `inbox.svg` | папка «Входящие» |
| `send.svg` | папка «Отправленные» |
| `drafts.svg` | папка «Черновики» |
| `archive.svg` | «Архив», папка «Архив» |
| `junk.svg` | папка «Нежелательная почта» |
| `folder.svg` | остальные папки |

## Лицензия

Fluent UI System Icons, https://github.com/microsoft/fluentui-system-icons, лицензия MIT:

```
MIT License

Copyright (c) 2020 Microsoft Corporation

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
