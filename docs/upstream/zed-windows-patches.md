# zed: Windows — патчи-кандидаты

Куда: zed-industries/zed — по PR на патч (не одним). Источник: [`pets-gpui-windows`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-windows), столбец
«Upstream» сверен с zed main 48741fd (2026-11-13). Не отправлять: ленивые текстуры путей (в zed уже иначе — тайлы),
`gpui_windows::pets` (наш API), порт gpui-fast (идея Longbridge), быстрый `layout_line` — после доводки
(разбор GSUB/GPOS/`kern`, бенч). Каждый PR — с замером из README крейта.

---

## 1. Compile shaders at build time with `D3DCompile`

**Title:** gpui_windows: compile HLSL in `build.rs` via `D3DCompile` instead of locating `fxc.exe`

**Summary:** build depends on finding `fxc.exe` / Windows SDK (`GPUI_FXC_PATH`); shaders could also be compiled at
startup (~250 ms). Calling `D3DCompile` (d3dcompiler_47) from `build.rs` removes the SDK tool dependency and
machine paths; bytecode is embedded, startup cost 0.4 ms. Non-Windows hosts (cross check) fall back to runtime
compilation (`cfg(shaders_runtime)`).
**Reproduction:** build on a machine without the Windows SDK `fxc.exe` on PATH → build fails or needs `GPUI_FXC_PATH`.
**Patch:** `build.rs`, `directx_renderer.rs`, `Cargo.toml` — https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-windows

## 2. Frames on demand: vsync thread sleeps without frame requests

**Title:** gpui_windows: vsync thread wakes every frame even when idle — sleep until a frame is requested

**Summary:** the vsync thread spins every display frame while the app is idle, costing CPU and battery. With frames on
demand it blocks until a window requests a frame.
**Reproduction:** idle gpui window on Windows → steady CPU wakeups at display rate (ETW / Process Explorer).
**Patch:** `vsync.rs`, `platform.rs`, `window.rs`, `events.rs`; recipe and measurement — https://github.com/Dicobraz421/pets/blob/0df0fe19023827d1ea31a47a998c335eebfa334c/docs/recipes/frames-on-demand.md

## 3. Live resize: draw on `WM_SIZE` inside the modal size loop

**Title:** gpui_windows: content lags/stretches during interactive resize — render on `WM_SIZE` in the size-move loop

**Summary:** during drag-resize frames come only from the size-move loop timer, so content lags behind the frame.
Rendering a frame on each `WM_SIZE` and creating the swapchain with stretch scaling keeps content in sync.
(We also default DirectComposition off — that part is our choice and is **not** proposed.)
**Reproduction:** drag a window edge quickly → content lags and black/stale bands appear.
**Patch:** `events.rs`, `window.rs`, `directx_renderer.rs`; recipe — https://github.com/Dicobraz421/pets/blob/0df0fe19023827d1ea31a47a998c335eebfa334c/docs/recipes/live-resize.md

## 4. Erase background with the last frame's background colour

**Title:** gpui_windows: `WM_ERASEBKGND` paints black during resize — use the root background colour

**Summary:** the class brush is black, so newly exposed areas flash black while resizing. Fill with the colour of the
first opaque full-window quad of the previous frame (the root `bg`); black only before the first frame.
**Reproduction:** light-themed window, enlarge it quickly → black flashes in new areas.
**Patch:** `events.rs`, `window.rs` (same PR as #3 is fine) — https://github.com/Dicobraz421/pets/blob/0df0fe19023827d1ea31a47a998c335eebfa334c/docs/recipes/live-resize.md

## 5. No black frame at window creation (`DWMWA_CLOAK`)

**Title:** gpui_windows: new window shows a black frame for 80–200 ms before the first render

**Summary:** cloak the window (`DWMWA_CLOAK`) from creation until the first frame is finished by the GPU and picked
up by DWM (query event + `DwmFlush` after `Present`; uncloaking right after `Present` still showed one black
vsync). Measured with Windows Graphics Capture: 80–200 ms of black → 0.
**Reproduction:** open a window with a light background and record it with WGC → leading black frames.
**Patch:** `window.rs` — https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-windows

## 6. `GPUI_FORCE_WARP=1`

**Title:** gpui_windows: allow forcing the WARP software adapter

**Summary:** for CI and VMs without a GPU: `GPUI_FORCE_WARP=1` creates the device on `EnumWarpAdapter` without
enumerating GPUs; read once per process, device recreation after loss also uses WARP.
**Reproduction:** CI runner / VM with a broken or absent GPU driver → device creation fails or picks a bad adapter.
**Patch:** `directx_devices.rs` — https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-windows

## 7. Quiet window close

**Title:** gpui_windows: closing a window logs 4–5 errors (`window not found`, `0x80070578`, `0x80040102`) and leaks the `IDropTarget`

**Summary:** after a window is removed from `App`, posted tasks, broadcasts and vsync redraws still target the dead
HWND, and `Drop` calls `RevokeDragDrop`/`DestroyWindow` on it. Fix: release callbacks when the window is removed
(`Callbacks::release`); revoke drag-drop and mark the window destroyed in `WM_DESTROY`, so `Drop` does not touch
a dead HWND; `ERROR_INVALID_WINDOW_HANDLE` / `DRAGDROP_E_INVALIDHWND` for a window that no longer exists
(`IsWindow`) logs at DEBUG, for a live one at ERROR. Also on close via the title-bar button the `IDropTarget`
holding the window's `Rc` was never released.
**Reproduction:** open and close a window, then quit → errors in the log and in Windows Event Log «Application».
**Patch:** `window.rs`, `events.rs`, `dispatcher.rs`, `platform.rs`, `util.rs` — https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-windows (pets #80)
