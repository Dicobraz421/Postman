**Что скачать** — Windows 10/11 x64

| Файл | Зачем |
|---|---|
| `postman-{version}-windows-x64.msi` | Установщик: для всех пользователей, в `Program Files`, нужны права администратора. Двойной щелчок — и подтвердить запрос UAC. |
| `postman-{version}-windows-x64.zip` | Программа целиком, установка только для себя, без прав администратора: распакуйте и выполните `install.ps1` — инструкция в `README.txt` внутри архива. |
| `postman-worker.exe`, `postman-viewer.exe` | Отдельные компоненты. Postman пишет «Нет компонента» — скачайте нужный файл и положите в папку программы, переустанавливать не нужно. |
| `SHA256SUMS.txt` | Контрольные суммы файлов. |

**Downloads** — `postman-{version}-windows-x64.msi` installs for all users (administrator rights);
`postman-{version}-windows-x64.zip` is the full program for the current user without administrator rights (unpack, run `install.ps1`); `postman-worker.exe` and `postman-viewer.exe` are single components to put into the program folder when Postman reports “Component missing”.
