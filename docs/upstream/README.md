# Отчёты upstream

Готовые тексты issue и PR в чужие репозитории. Отправляет владелец: у сессий нет доступа к чужим репозиториям.
Тексты — по-английски; наши патчи — pets @0df0fe1 (пин Postman). После отправки — ссылку и статус в таблицу;
после принятия upstream — убрать патч из форка (README крейта pets, столбец «Upstream»).

| Файл | Куда | Суть | Статус |
|---|---|---|---|
| [zed-linux-emoji-xdnd.md](zed-linux-emoji-xdnd.md) | zed-industries/zed, 2 issue / PR | Linux: цветные эмодзи (порядок fallback); X11 XDND drag-out | не отправлено |
| [zed-windows-patches.md](zed-windows-patches.md) | zed-industries/zed, 7 PR | Windows: шейдеры при сборке, кадры по запросу, живой ресайз, фон, без чёрного кадра, WARP, тихое закрытие | не отправлено |
| [gpui-kit-html5ever.md](gpui-kit-html5ever.md) | longbridge/gpui-kit, PR | `gpui-base`: html5ever 0.39 | не отправлено |

Патчи: [`pets-gpui-macos-platform`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-macos-platform), [`pets-gpui-wgpu`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-wgpu),
[`pets-gpui-linux`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-linux), [`pets-gpui-windows`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-windows),
[`pets-gpui-base`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-base). Снимок zed — d2a4fb6 (`gpui-pre-*` 0.3.12); Windows сверен с zed main 48741fd.
Перед отправкой — проверить, не исправлено ли уже в zed main.
