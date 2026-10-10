# HO5 source and Linux validation

This is a redacted reproducibility summary. Raw host paths, boot/process IDs,
operation handles and logs remain local. Linked binary and actual portal
attempts are recorded in [HO5-LOCAL-PORTAL-ACCEPTANCE.md](HO5-LOCAL-PORTAL-ACCEPTANCE.md).

Implementation `90fcce0a474abcd29369f216e331d7d240fecc30` was committed,
pushed to `codex/gpui-mvvm-migration` and fetched into the existing HO5 checkout.
The previous `main@969ca109` ref was retained. All 938 source manifest files
matched; 891 changed inputs had their mtimes refreshed before building.
The existing `target/` was reused. Support commit
`c4d7031e29e5aa11d18c92e8954603debd394194` adds the lifecycle checker/tests;
its 940-file source digest is
`ebf66ad7c83018eb80de285eb7c964da78b6f73c3423956a44656db6752d28d0`.
Tested Rust inputs are unchanged between these commits.

## Completed checks

- `cargo check -p host --lib --locked`: passed. Initial offline cache lacked
  PipeWire; a normal locked download using the existing registry resolved it,
  without changing the lockfile.
- `cargo check -p remote_play_app --bin remote_play --locked --offline`: passed.
- The same check with `--features native-linux-video`: passed, including the
  native C ABI bridge and fixed GPUI/Blade interop. A check alone does not prove
  linkage, frames or native presentation.
- `cargo test -p host --lib linux_pipewire::tests --locked --offline`: 11 passed,
  zero failed/ignored. Initial linking lacked Opus: its existing generated
  archive was in `out/lib64` while the dependency emitted `out/lib`.
  Command-local `LIBRARY_PATH` pointed to that generated `out/lib64`;
  no system symlink or source change was made.

The existing FFmpeg 8.1.2 archive has SHA-256
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`.
Signature verification matched the
[official release key](https://ffmpeg.org/download.html), fingerprint
`FCF986EA15E6E293A5644F10B4322F04D67658D8`. 905 codec/util source headers
matched the signed archive; avconfig.h was generated separately.
Headers are codec 62.28.102/util 60.26.102; runtime calls report
62.28.103/60.26.103. System FFmpeg/drivers were not replaced.

## Acceptance boundaries

The existing product service remained active. Reading the actual process
executable was denied; disk-file queries do not establish its loaded build.
No service restart or candidate installation occurred. Actual capture, native
presentation, input/audio/files, visual comparison and sustained performance
remain pending. Mac/Windows acceptance is separate.

The no-window GUI gate checks compiled renderer/features. ProductInfo and
per-profile owner metadata still omit compiled commit/build, so the requested
complete source/build/process startup chain is unfinished. The read-only
lifecycle helper has six regression tests, rejects unconditional GUI restart
policy and permits headless background KeepAlive; it is not integrated into
installed launchers and does not stop any service.

Required local Python regression was **197 total: 191 passed, 6 skipped, 0 failed**.
It includes preserved Android release tests outside the Linux commit. Native
signing integration was disabled. No passing test was repeated for documentation.

[Redacted machine-readable summary](evidence/ho5-candidate-validation.json).
