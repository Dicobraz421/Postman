# Наши патчи gpui

С 2026-10-19 (S2-07) патч живёт в репозитории `pets` — `crates/pets-gpui-windows` (пакет `gpui-pre-windows`
0.3.9), Postman подключает его `[patch]` на ревизию `pets`. Таблица ниже — история и состав; актуальный список —
`pets/crates/pets-gpui-windows/README.md`.
Каждый патч в коде помечен «Postman patch». При обновлении gpui-kit переносим непринятые патчи.

С T3-06 и T3-07 (2026-11-11) — второй патч там же: `crates/pets-gpui-base` (пакет `gpui-base` 0.7.3 gpui-kit,
`[patch.crates-io]` в `Cargo.toml`) — блоки в тексте поля, как в Zed: таблица сеткой и картинка в письме; выделение,
отмена, IME и вставка — прежние. Состав и upstream — `pets/crates/pets-gpui-base/README.md`.

| Патч | Файлы | Зачем | Upstream |
|---|---|---|---|
| Шейдеры в exe | `build.rs`, `directx_renderer.rs` | кросс-сборка из Linux без fxc.exe, в бинаре нет путей машины сборки | не предлагаем; уйдёт, когда сборка будет только на Windows |
| Кадры по запросу (S1-02) | `vsync.rs`, `platform.rs`, `window.rs`, `events.rs` | поток `VSyncProvider` инвалидировал все окна на каждом обновлении экрана (144–180 раз в секунду) — ~3 % CPU в простое. Теперь окно просит кадр через `frame_waker`/`schedule_frame` (ядро gpui их уже вызывает), поток спит, пока запросов нет, и раз в секунду проверяет устройство и остановку | предложить в zed-industries/zed |
| Живой ресайз (S1-04) | `events.rs`, `window.rs`, `directx_renderer.rs`, `platform.rs` | при перетаскивании края содержимое отставало от рамки, в новой области — чёрная полоса. Внутри цикла перетаскивания кадр рисуется сразу на `WM_SIZE`; swapchain окна с `DXGI_SCALING_STRETCH`; DirectComposition выключена по умолчанию (`GPUI_DISABLE_DIRECT_COMPOSITION=0` — включить) | первые два — предложить в zed-industries/zed; умолчание без DirectComposition — только у нас (у Zed прозрачный фон) |
