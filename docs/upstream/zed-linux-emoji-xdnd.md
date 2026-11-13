# zed: Linux — цветные эмодзи и X11 XDND drag-out

Куда: zed-industries/zed — два отдельных issue (лучше сразу PR). Наши патчи: [`pets-gpui-wgpu`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-wgpu)
(`src/pets_emoji.rs`, `src/cosmic_text_system.rs`) и [`pets-gpui-linux`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-linux)
(`src/linux/x11/pets_xdnd.rs`, `x11/client.rs`, `x11/window.rs`). Postman: INT-01 (#638).

---

## 1. Color emoji fall back to monochrome glyphs on Linux

**Title:** Linux: emoji render as monochrome outlines (or not at all) — color emoji font is last in the fallback list

**Summary**

`gpui_wgpu`'s cosmic-text system takes fallback fonts from cosmic-text's generic Unix list, where Noto Color Emoji
comes after DejaVu Sans and Noto Sans Symbols2. Graphemes with emoji presentation therefore get a monochrome glyph
from a symbol font, or nothing. Setting the emoji font as a fallback in `Font` does not help: `load_family`
drops faces without an `m` glyph.

**Reproduction**

1. Linux (X11 or Wayland) with `fonts-noto-color-emoji` and DejaVu installed.
2. Render text `"Hello 😀 👍🏽 1️⃣ 🇩🇪 👨‍👩‍👧"` in any gpui view.
3. Emoji are drawn as outlines from DejaVu / Symbols2, or as tofu; in other Linux apps they are in color.

**Suggested fix**

Before generic fallback, split out graphemes with emoji presentation (UTS #51: `Emoji_Presentation`, `U+FE0F`,
keycap sequences, flags, ZWJ sequences) and give them a run in a known color emoji font (Noto Color Emoji, Twemoji,
JoyPixels, …) if one is installed and covers the grapheme; everything else keeps the current fallback.
Our patch with tests (`cargo test --test pets_emoji`): https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-wgpu

## 2. X11: no external drag (drag-out) source

**Title:** Linux X11: `start_external_drag` is Wayland-only — files cannot be dragged out of a window on X11

**Summary**

Dragging files out of a gpui window to another app works on Wayland (`wl_data_device`) but does nothing on X11:
`X11Window` has no `start_external_drag`.

**Reproduction**

1. X11 session (or Xvfb).
2. A view with `on_drag(...)` + `external_drag_payload` carrying file paths.
3. Drag outside the window onto a file manager — nothing is dropped. Same code works on Wayland.

**Suggested fix (XDND source)**

- `X11Window::start_external_drag`: the source window owns `XdndSelection`, offers `text/uri-list` and
  `XdndActionCopy`.
- Targets are found via `XdndAware` under the pointer (accept any property type: gpui itself sets it with type
  `XA_ATOM`); positions wait for `XdndStatus`; Esc cancels; the end of the session sends `FileDropEvent::Ended`
  to the window. Own windows are not treated as targets.

Our implementation with tests (`--features test-support -- pets_`) and an Xvfb check: https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-linux
