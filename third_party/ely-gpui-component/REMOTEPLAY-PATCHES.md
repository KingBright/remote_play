# RemotePlay CE core profile

Source: [Ely-GPUI-Components](https://github.com/ZacharyZhang-NY/Ely-GPUI-Components/tree/f756043853ca93407e2da5d07cfe520c84f90963), revision `f756043853ca93407e2da5d07cfe520c84f90963`.
`UPSTREAM.json` records original SHA-256 values for every selected upstream file.
`Cargo.toml.orig`, `AGENTS.md`, `LICENSE`, and all asset licenses retain upstream content.
This is a selected, patched core profile, not an unmodified full Ely distribution.

## Original implementations retained

344 selected upstream files remain byte-identical, including assets/fonts/icons and their
licenses, icon implementation, spinner/curves, typography keys, palette, interaction,
chart and other theme token modules. The selected Button, FocusScope and Theme implementations
remain upstream-derived with the four explicit patches below. No full gallery, forms,
terminal, WebView, Android/Web backend, or catalog dependencies are included.

## Compatibility changes

- `Cargo.toml`: one `gpui-ce =0.3.3` dependency, no gpui-pre/platform dependency;
  compile only `src/remoteplay.rs`. The core profile compiles on the existing Rust 1.94.1;
  upstream's full crate still declares Rust 1.95. `rust-embed =8.11.0` uses the existing
  cached version rather than upstream 8.12. No toolchain replacement or version drift.
- `src/remoteplay.rs`: explicit module/export profile; validate assets and register original
  fonts before initializing the single Ely Theme and ElyFocus key bindings. Test initialization
  avoids native font registration; the product always uses fallible full initialization.
- `src/primitives/focus.rs`: use CE focus/focus-next/focus-prev signatures without an App
  argument. Root, trap, disabled tab-stop and focus restoration semantics stay upstream-derived.
- `src/theme/tokens.rs`: construct CE BoxShadow fields rather than a pre-only constructor.
- `src/theme/mod.rs`: handle CE AsyncApp::update's Result in the theme animation loop;
  log a lost animation owner and stop instead of interpreting an error as a running owner.
- `src/buttons/button.rs`: expose GPUI Styled/InteractiveElement/ParentElement through a
  stored Div, preserve custom children and final product style refinements; accept a variant
  adapter. Rename ControlSize setter to `control_size` so GPUI `.size(Pixels)` remains usable.
  Disabled buttons keep no click handler or tab stop; upstream loading/hover/press/focus logic
  remains. Product glyphs and original compact layouts are supplied by the host adapter.

## Accessibility gap

CE 0.3.3 has no upstream pre Role/ARIA APIs. Button role, aria_label and aria_description
calls are explicitly omitted, including the disabled aria description/i18n use. This is an
unimplemented accessibility capability, not complete Ely API compatibility or a11y acceptance.
Keyboard focus has mock-window tests; native accessibility and assistive technology remain
unverified. Do not represent this profile as upstream's full native/Web accessibility support.

## Attribution

Ely remains MIT, Copyright (c) 2026 Ely GPUI Component contributors. Fonts retain their
OFL notices and Lucide icons their ISC notice in assets. RemotePlay changes are recorded
here; original file identities/hashes are not replaced by local hashes.
