# LIN-06. Выпуск Postman для Linux ставится и запускается

Статус: ⏳ (стенда с Linux нет — решение 2026-11-12 «linux»; появится — GNOME на Wayland и KDE)

x86_64: Ubuntu 24.04 (GNOME) и Fedora (KDE). Файлы — со страницы выпуска или из артефакта прогона CI
`postman-<версия>-linux-x64` (до тега).

1. deb (Ubuntu): `sudo apt install ./postman-<версия>-linux-x64.deb`. Ожидается: Postman в меню приложений со
   значком; `ls /etc/apparmor.d/postman` — файл есть; `sudo aa-status | grep postman` — профили `postman-worker` и
   `postman-viewer`.
2. Запуск из меню. Ожидается: окно, демо-ящик; диагностика песочницы (`docs/DEV.md` «Диагностика песочницы») —
   namespaces включены, в журнале нет WARN о запрете user namespaces.
3. Ссылка `mailto:` в браузере. Ожидается: предлагается Postman (или открывается новое письмо, если он выбран по
   умолчанию: `xdg-mime query default x-scheme-handler/mailto` → `postman.desktop`).
4. Удаление: `sudo apt remove postman`. Ожидается: пункта меню нет, `/etc/apparmor.d/postman` нет; профиль данных
   остаётся.
5. rpm (Fedora): `sudo dnf install ./postman-<версия>-linux-x64.rpm`, шаги 2–4 (`dnf remove postman`). AppArmor в
   Fedora нет — песочница без ограничений дистрибутива.
6. AppImage (любой дистрибутив): `chmod +x postman-<версия>-linux-x64.AppImage && ./postman-<версия>-linux-x64.AppImage`.
   Ожидается: окно; на Ubuntu 23.10+ в журнале WARN — песочница без user namespaces (профиля AppArmor у AppImage нет).
