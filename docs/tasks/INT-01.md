# INT-01: пин pets и интеграция Linux (перетаскивание, эмодзи, обновление, экземпляр)

lane: x
status: done
deps: [PAR-06]
class: B
files: [Cargo.toml, Cargo.lock, app-gpui/Cargo.toml, app-gpui/src/attachments.rs, app-gpui/src/sys/mod.rs, app-gpui/src/update.rs, app-gpui/src/settings_general.rs, app-gpui/src/main.rs, app-gpui/i18n/]
read: [docs/adr/0011-platform-contract.md, docs/tasks/PAR-04.md]
checks: [clippy, nextest, ci-linux]
manual: no

## Что сделано
- Все pets-крейты и `[patch.crates-io]` (`gpui-base`, `gpui-pre-windows`, `gpui-pre-linux`, `gpui-pre-wgpu`) — один
  коммит main pets `4034604` (#74 GLIN-01, #75 PAR-03, #77 UPD-LIN, #76 PAR-07). Windows: копия для перетаскивания — `mark_from_internet`, не записалась — не тащим.
- Эмодзи на Linux — цветным шрифтом (форк `pets-gpui-wgpu`): снимок `preview-pdf`, тема «Обновление регламента 📋».
- Перетаскивание вложения из окна на Linux — `gpui_linux::pets::drag` (`on_drag` + `external_drag_payload`; X11 XDND,
  Wayland — upstream): копия в `$TMPDIR/Postman/drag` готовится заранее, как на macOS; копии нет — не тащим (PAR-04 №11).
- Обновление на Linux: вид — `InstallKind::current_linux()` (AppImage, deb, rpm; сборка из исходников — без обновления);
  deb/rpm — «Скачать» (`update-download`) открывает `asset_url`; AppImage — установка, как portable.
- `single_instance`: ошибка — `fatal` (pets PAR-03, решение pets 2026-11-13), на всех ОС.

## Договор платформенных функций (ADR 0011)

| функция | отказ | журнал | пределы и права | на диске | политики | Fluent | отличие ОС и причина |
|---|---|---|---|---|---|---|---|
| Перетаскивание наружу | закрыто: копии нет — не тащим, тост «можно перетаскивать» по готовности | ERROR копия не создана | копия — только своё вложение, безопасное имя | `$TMPDIR/Postman/drag`, чистка при старте и выходе | — | `att-drag-ready`, `att-drag-failed` | сеанс ведёт gpui (Windows — OLE, macOS — NSDraggingSession) |
| Обновление | deb/rpm: установить нельзя без root — ссылка «Скачать»; неизвестный вид — без обновления | как прежде | — | AppImage — замена рядом | `AllowUpdate` | `update-download` | deb/rpm — пакетный менеджер (root), как MSI для всех пользователей на Windows требует UAC |
| Единственный экземпляр | закрыто: ошибка проверки — сообщение и выход | ERROR (`fatal`) | замок/сокет — каталог только владельца (pets) | — | — | `fatal-single-instance` | — |

## Проверки
- clippy `-D warnings` linux (вся рабочая область) и `x86_64-pc-windows-msvc` (`postman-gpui`, `postman-core`);
  nextest `postman-gpui` + `postman-core` — 609 passed; снимки Xvfb до/после — эмодзи есть.
