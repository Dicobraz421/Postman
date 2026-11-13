# longbridge/gpui-kit: gpui-base на html5ever 0.39

Куда: longbridge/gpui-kit, PR (или issue, если PR от владельца нежелателен). Наш патч:
[`pets-gpui-base`](https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-base) (`Cargo.toml`, `Cargo.lock`; код не менялся).

---

**Title:** gpui-base: bump html5ever to 0.39 (and markup5ever_rcdom)

**Summary**

`gpui-base` 0.9.1 still depends on `html5ever` 0.27 + `markup5ever_rcdom` 0.3. Apps that also parse HTML with a
current html5ever get two copies of the whole HTML stack (html5ever, markup5ever, xml5ever, tendril, string_cache,
phf) in the binary. Moving to `html5ever` 0.39 + `markup5ever_rcdom` 0.39.0+unofficial needs no code changes —
the API used by `gpui-base` is unchanged.

**Reproduction**

1. App depending on `gpui-base` 0.9.1 and `html5ever` 0.39.
2. `cargo tree -d -i html5ever` → two versions (0.27 and 0.39) and duplicates of markup5ever, tendril, string_cache, phf.

**Change**

`crates/base/Cargo.toml`: `html5ever = "0.39"`, `markup5ever_rcdom = "0.39.0+unofficial"`; update `Cargo.lock`.
Tests of the crate pass unchanged. Our snapshot with the bump: https://github.com/Dicobraz421/pets/tree/0df0fe19023827d1ea31a47a998c335eebfa334c/crates/pets-gpui-base
