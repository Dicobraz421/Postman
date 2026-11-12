<!-- Страница выпуска для пользователей: job `publish` (ci.yml) вырезает лишние блоки и подставляет {version}. -->
<!-- pre -->
**Бета-версия.** Основное работает, но возможны ошибки. Нашли ошибку — «Настройки» → «Общие» → «Отчёт для поддержки» и приложите файл к сообщению.
<!-- /pre -->
<!-- stable -->
**Стабильная версия** для повседневной работы.
<!-- /stable -->

## Скачать — Windows 10 и 11 (64 бита)

| | Что это | Права |
|---|---|---|
| [**Установщик** (.msi)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-windows-x64.msi) | Обычный выбор. Откройте файл — Postman установится для всех на этом компьютере и появится в меню «Пуск». | Нужны права администратора: Windows попросит подтвердить. |
| [**Архив** (.zip)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-windows-x64.zip) | Если прав администратора нет. Распакуйте и запустите `install.ps1` — как, написано в `README.txt` внутри. Postman установится только для вас. | Не нужны. |

Обновление — так же: новая версия ставится поверх старой, письма и вход сохраняются. Перед обновлением выйдите из Postman: «Выход» в меню его значка в трее (крестик только прячет окно).
<!-- unsigned -->

Если Windows пишет «Система Windows защитила ваш компьютер», нажмите «Подробнее» → «Выполнить в любом случае».
<!-- /unsigned -->

Остальные файлы ниже (`postman-worker.exe`, `postman-viewer.exe`) нужны, только если Postman на Windows сам попросит скачать недостающий компонент.

## Скачать — macOS 13 и новее, Apple Silicon (M1 и новее)

| | Что это |
|---|---|
| [**Образ диска** (.dmg)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-macos-arm64.dmg) | Обычный выбор. Откройте файл и перетащите **Postman** на ярлык **Applications** («Программы») в том же окне. |
| [**Архив** (.zip)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-macos-arm64.zip) | То же приложение: распакуйте двойным щелчком и перенесите `Postman.app` в «Программы». |

На Mac с процессором Intel Postman не работает.

Обновления Postman ставит сам: когда выйдет новая версия, он предложит её установить («Настройки» → «Общие» → «Проверить обновления» — сразу). Письма и вход сохраняются.
<!-- mac-unsigned -->

**Первый запуск.** Подписи Apple у Postman пока нет, поэтому macOS при первом открытии откажет: «Не удаётся проверить разработчика» или «Postman повреждён». Это нужно сделать один раз:

1. Откройте Postman из «Программ» — появится отказ, нажмите «Готово».
2. «Системные настройки» → «Конфиденциальность и безопасность» → внизу «Postman заблокирован…» → **«Всё равно открыть»**, подтвердите паролем или Touch ID.
3. Снова откройте Postman → «Открыть».

Или одной командой в Терминале: `xattr -dr com.apple.quarantine /Applications/Postman.app`
<!-- /mac-unsigned -->

*English:* Windows — the **.msi** installs Postman for all users (administrator rights required); the **.zip** installs it for you only, no administrator rights needed. macOS (Apple Silicon, 13+) — open the **.dmg** and drag Postman onto Applications.
<!-- mac-unsigned -->
The app is not signed by Apple yet: on first launch go to System Settings → Privacy & Security → "Open Anyway", or run `xattr -dr com.apple.quarantine /Applications/Postman.app`.
<!-- /mac-unsigned -->
