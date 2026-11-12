Postman — почта, календарь и контакты для macOS 13 и новее, Apple Silicon (M1 и новее)

Сборка для проверки (волна MAC-01): демо-ящик и снимки работают; вход в Связку ключей, Cmd-клавиши,
уведомления, mailto и прочее системное — в следующих волнах.

Файлы:
  Postman.app  — приложение; внутри (Contents/MacOS) — postman, postman-worker (изолированный разбор писем,
                 песочница Seatbelt) и postman-viewer (просмотр вложений модулями, песочница)
  README.txt   — этот файл

Новые версии: https://github.com/Dicobraz421/Postman/releases

УСТАНОВКА

1. Распакуйте архив postman-<версия>-macos-arm64.zip (двойной щелчок в Finder).
2. Перетащите Postman.app в «Программы».

ПЕРВЫЙ ЗАПУСК

Подписи Apple у сборки пока нет (подпись ad-hoc), поэтому macOS при первом запуске откажет:
«Не удаётся проверить разработчика» или «Postman повреждён».
  1. Откройте Postman.app двойным щелчком — появится отказ, нажмите «Готово».
  2. «Системные настройки» → «Конфиденциальность и безопасность» → внизу «Postman заблокирован…» →
     «Всё равно открыть», подтвердите паролем или Touch ID.
  3. Снова откройте Postman.app → «Открыть».
Или в Терминале, один раз:  xattr -dr com.apple.quarantine /Applications/Postman.app

ЗАПУСК

Демо без сервера:  /Applications/Postman.app/Contents/MacOS/postman --demo
Параметры:         /Applications/Postman.app/Contents/MacOS/postman --help

ДАННЫЕ

  ~/Library/Application Support/Postman — настройки, кэш писем, поисковый индекс;
  ~/Library/Logs/Postman — журнал.
Удаление: перенесите Postman.app в Корзину; данные — эти папки, вручную.
