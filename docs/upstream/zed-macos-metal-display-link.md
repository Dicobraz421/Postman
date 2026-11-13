# zed: macOS — CAMetalDisplayLink, пауза не снимается

Куда: zed-industries/zed, issue (label `gpui`, `platform:macos`). Наш патч: [`pets-gpui-macos-platform`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-macos-platform)
(CVDisplayLink по умолчанию, Metal — `GPUI_METAL_DISPLAY_LINK=1`; диагностика — цель `gpui_macos::frames`).
Postman: #642 (GMAC-01). Перед отправкой — приложить журнал прогона снимков с Metal-путём (см. «Воспроизведение»).

---

**Title:** macOS: window stops receiving frames for 7–44 s with `CAMetalDisplayLink`; `request_animation_frame` does not resume it

**Summary**

On macOS 14+ a window driven by the `CAMetalDisplayLink` frame source (`gpui_macos`, zed@d2a4fb6, published as
`gpui-pre-macos` 0.3.12) sometimes gets no frame callbacks for 7–44 s although the app keeps asking for frames
(`window.request_animation_frame()` and `on_next_frame` every frame). Frames resume only on the next system
`displayLayer:` — often on a minute boundary. With the `CVDisplayLink` source (as in 0.3.9) the same app never
stalls: 17/17 CI runs without stalls vs 4/6 runs with stalls on the Metal path.

**Reproduction**

1. App on gpui 0.3.12, macOS 14+ (GitHub `macos-26` runner, Apple Silicon).
2. Open a window and render a sequence of scenes, each waiting for the next frame (`request_animation_frame` +
   `on_next_frame`) before taking a screenshot.
3. In some runs one scene waits 7–44 s; the frame then arrives together with `displayLayer:`.
   No stall with `CVDisplayLink`.

**Where it can come from (code reading, no proven root cause yet)**

- `MetalFrames::end_frame` pauses the link after an update in which gpui neither drew nor woke it. Only `wake()`
  resumes it (invalidator waker, `schedule_frame`, `on_next_frame`). The invalidator waker fires only on a
  clean→dirty transition: if the pause happens while the window is already dirty, no wake follows.
- `wake()` is a silent no-op when the link is detached (`stop_display_link` on occlusion, `display_layer`,
  activation) or currently borrowed (`try_borrow` fails).
- `start_display_link` returns early for a window AppKit reports as not visible; then only system redraws drive frames.

`CVDisplayLink` ticks continuously while the window is visible, so none of these paths can stop it.

**Suggested fix**

- Do not pause the link while the window is dirty or has pending `on_next_frame` callbacks: check state at
  `end_frame`, not only at the clean→dirty edge.
- Make `wake()` record a pending wake when the link is borrowed or detached and honour it on the next
  `start_display_link` / after the borrow ends.
- Restart the link on visibility change instead of returning early in `start_display_link`.

Until fixed, falling back to `CVDisplayLink` (or an env switch) avoids the stall. Our workaround and
debug logging (`RUST_LOG=gpui_macos::frames=debug`: pause, detach, skipped start, wake without a live link):
https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-macos-platform
