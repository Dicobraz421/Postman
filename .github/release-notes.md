<!-- Страница выпуска для пользователей: job `publish` (ci.yml) вырезает лишние блоки и подставляет {version}. -->
<!-- pre -->
**Бета-версия.** Основное работает, но возможны ошибки. Нашли ошибку — «Настройки» → «Общие» → «Отчёт для поддержки» и приложите файл к сообщению.

**Бета-тестерам.** Версия 0.1.0-beta.5 предложит эту версию сама (канал «Предварительный»). Версии 0.1.0-beta.4 и старше обновить себя не умеют: один раз установите эту версию вручную (файлы ниже) — письма и вход сохранятся, — затем «Настройки» → «Общие» → «Обновления» → канал **«Предварительный»**.
<!-- /pre -->

## Что нового

- **Версия для Linux** (предварительно) — пакеты для Ubuntu и Debian, Fedora, RHEL и openSUSE и AppImage для любого дистрибутива. Файлы — ниже.
- **На Mac — подпись и шифрование писем S/MIME** ключами из «Связки ключей», вход в Exchange и в адресную книгу компании без пароля — по билету Kerberos.
- **Строка меню на Mac** и число непрочитанных на значке в Dock.
- **«Свои сертификаты»** в «Настройки» → «Безопасность»: кому выдан, срок, годится ли для подписи; на Linux — добавить сертификат из файла .p12 или .pfx.
- **Подпись S/MIME**: сертификат, срок которого ещё не начался, отмечается «ещё не действует»; промежуточные сертификаты Postman берёт из самих писем.
- **Политики организации** — одни и те же ключи и значения на всех системах.
- **Песочница строже**: письма и вложения разбираются в более закрытом процессе на всех системах.
<!-- stable -->
**Стабильная версия** для повседневной работы.
<!-- /stable -->

## Скачать — Windows 10 и 11 (64 бита)

| | Что это | Права |
|---|---|---|
| [**Установщик** (.msi)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-windows-x64.msi) | Обычный выбор. Откройте файл — Postman установится для всех на этом компьютере и появится в меню «Пуск». | Нужны права администратора: Windows попросит подтвердить. |
| [**Архив** (.zip)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-windows-x64.zip) | Если прав администратора нет. Распакуйте и запустите `install.ps1` — как, написано в `README.txt` внутри. Postman установится только для вас. | Не нужны. |

Обновления Postman ставит сам и спрашивает перед установкой. Вручную — так же, как установка: новая версия ставится поверх старой, письма и вход сохраняются. Перед этим выйдите из Postman: «Выход» в меню его значка в трее (крестик только прячет окно).
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

## Скачать — Linux (x86_64)

Предварительно: Linux пока проверяется только автоматически. Нужен 64-битный дистрибутив не старше Ubuntu 22.04, Debian 12, Fedora 35 или RHEL (Alma, Rocky) 9.

| | Что это |
|---|---|
| [**Пакет для Ubuntu и Debian** (.deb)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-linux-x64.deb) | `sudo apt install ./postman-{version}-linux-x64.deb` — Postman появится в меню приложений. |
| [**Пакет для Fedora, RHEL и openSUSE** (.rpm)](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-linux-x64.rpm) | `sudo dnf install ./postman-{version}-linux-x64.rpm` (openSUSE — `sudo zypper install …`). |
| [**AppImage**](https://github.com/Dicobraz421/Postman/releases/download/v{version}/postman-{version}-linux-x64.AppImage) | Один файл для любого дистрибутива, без установки: сделайте его исполняемым (`chmod +x`) и запустите. |

Пакеты .deb и .rpm лучше AppImage на Ubuntu 23.10 и новее: с ними вложения открываются в более строгой изоляции. Обновления: AppImage Postman обновляет сам; с пакетами .deb и .rpm он сообщит о новой версии и даст ссылку «Скачать» — поставьте её так же, как при установке, письма и вход сохранятся.

*English:* New: a Linux version (preview), S/MIME and passwordless Kerberos sign-in on Mac. Postman updates itself (Settings → General → Updates; from 0.1.0-beta.4 and older, install this version manually once and pick the "Preview" channel). Windows — the **.msi** installs Postman for all users (administrator rights required); the **.zip** installs it for you only, no administrator rights needed. macOS (Apple Silicon, 13+) — open the **.dmg** and drag Postman onto Applications. Linux (x86_64, preview) — the **.deb** (Ubuntu, Debian) or **.rpm** (Fedora, RHEL 9, openSUSE) package, or the **.AppImage** for any distribution; the AppImage updates itself; with .deb and .rpm Postman offers a download link for the new version.
<!-- mac-unsigned -->
The app is not signed by Apple yet: on first launch go to System Settings → Privacy & Security → "Open Anyway", or run `xattr -dr com.apple.quarantine /Applications/Postman.app`.
<!-- /mac-unsigned -->
