# GKIT-02: Postman на gpui-kit 0.9.1 / gpui-pre 0.3.12

lane: x/gkit-09             # app-gpui, Cargo.toml/lock, crates/{export,worker}/Cargo.toml (cfb) — класс B, согласовано
status: in-progress
deps: [pets GKIT-01]        # форки gpui-base, gpui-pre-windows и pets-крейты на 0.9.1 — один коммит pets
class: B
files: [Cargo.toml, Cargo.lock, app-gpui/Cargo.toml, crates/export/Cargo.toml, crates/worker/Cargo.toml, app-gpui/src/commands.rs, app-gpui/src/settings_keys.rs, app-gpui/src/mailer.rs, app-gpui/src/screens.rs]
read: [upstream longbridge/gpui-kit v0.7.3..v0.9.1]
checks: [fmt, clippy linux, clippy x86_64-pc-windows-msvc, nextest, снимки Xvfb до/после]
manual: yes                 # docs/manual-tests/GKIT-02.md

> Решение владельца 2026-11-12: перевести Postman с gpui-kit 0.7.3 / gpui-pre =0.3.9 на gpui-kit 0.9.1 / gpui-pre =0.3.12.

## Сделано
- gpui-kit 0.9.1, gpui-pre-windows =0.3.12; `[patch]` `gpui-base`, `gpui-pre-windows` и все pets-крейты — один коммит
  `github.com/Dicobraz421/pets` (было три rev и два адреса).
- Поломки API upstream: `PlatformInput::ScrollWheel(ScrollWheelEvent)` → `Scroll(ScrollEvent { source, .. })`;
  `KeyBinding::load(.., cx)` вместо `&DummyKeyboardMapper`; `Keystroke::unparse(os)`; `Keystroke::parse` только
  `'static` → `parse_with_os(.., os)` в сценах.
- jiff — в `[workspace.dependencies]`, только встроенная база IANA (`tzdb-bundle-always`, без `tzdb-zoneinfo`):
  офлайн и как на Windows; на Ubuntu 24.04 без `tzdata-legacy` `jiff_knows_the_main_zones` проходит.
- `cfb` 0.15 → 0.16 в `postman-export` и `postman-worker` (одна версия с `msi`).

## Не сделано
- Windows-снимки сцен (`--screens`) — только на стенде, `docs/manual-tests/GKIT-02.md`.
